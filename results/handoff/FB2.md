# FB2 — placeholder is not a pass; a screenshot through the observe verbs

**Branch:** `fb2-shot-placeholder` (off `screenshot-compress` `9bc91c2`, merged
`main` `74e4ee9` = v0.2.0).
**Worktree:** `/home/Qui/.paseo/worktrees/3oxa6xao/gr4`.
**Target:** `.cache/gr4-target` (release).
**Feedback:** `ACOS_AGENT_FEEDBACK.md`, subagent entry 1 items 4 and 5, entry 2
item 3-style evidence, and the **Triage** FB2 line.
**Date:** 2026-11-15.

> **Triage FB2 (owner priority, agent feedback).** screenshot through MCP
> (`acos_observe_tree {"screenshot": true}` → path + preview), NO_FRESH_FRAME
> instead of a silent no-op, placeholder-that-never-mounts → `blocked:
> placeholder-content` (not pass), missing-native-lib cross-check in inspect.

**Status: PARTIAL.** Items (2) and (1) are implemented with tests and committed.
The APK-level acceptance (crafted Gaten APK, 15 gates, 5 Gaten flows + 5
negatives, live MCP smoke on Gaten) was **not run**: the 60-minute budget went on
(2) and (1). Item (3) (`acos_inspect` native-lib cross-check) is **not started**.
Nothing here weakens an assertion, a gate or a status: every change makes a
non-pass more likely, never a pass.

---

## 1. Item (2) — a launch that never mounts is `blocked`, not `pass`

### The false green

`acos serve`'s launch verb (`crates/acos-cli/src/engine/serve.rs`,
`launch_reply`) hardcoded

```text
"stage_reached": "first_frame",
"stages": [{process ok}, {activity_resumed ok}, {first_frame ok}]
```

and returned `Envelope::pass` unconditionally — even when
`first_frame.placeholder: true` was in the same reply (`frame.meaningful:
false` was set, and nothing read it). That is the reported case: `pass`,
`first_frame`, 9-node empty RN surface, `pixels: false`, `frame_number` stuck at
1 for 40+ s while SoLoader looped on a missing `libworklets.so`.

The `acos verify --engine art` route had the same hole one layer down:
`Backend for ArtSession::launch` (`crates/acos-cli/src/engine/art.rs`) returned
`Ok(LaunchOutcome)` for a placeholder frame and only annotated
`detail.first_frame.placeholder`.

### The fix

* **New protocol code** `ErrorCode::PlaceholderContent` → `"PLACEHOLDER_CONTENT"`,
  `default_status = Status::Blocked`, class `harness` ("needs triage before it
  can be attributed": app bug vs missing native lib), in `ALL_ERROR_CODES`, with
  an agent-facing hint that names the missing-native-library route and
  `acos_inspect`. Schemas regenerated (`UPDATE_SCHEMA=1`): `envelope.json`,
  `protocol-error.json`, `verdict.json`, `artifacts-validation.json`.
* **serve launch reply:** if `session.first_frame_report().placeholder`, the
  reply is `Envelope::failure(status = blocked)` with
  * `code: PLACEHOLDER_CONTENT`, `details.reason: "placeholder-content"`,
  * `details`: `nodes`, `traversals`, `event_waits`, `waited_ms`, `settled`,
    `bound`, `frame_number`, `session_id`, and `log_hint`,
  * a hint that quotes the log line, and
  * the full ladder in `result` (so a caller still sees where it stopped):
    `stage_reached: "window"`, `content: {mounted: false, reason:
    "placeholder-content"}`, and the `first_frame` stage rewritten to
    `{ok: false, status: "blocked", blocker: {...}}`.
* **log hint** (`placeholder_hint_line`): the first line matching, in order,
  `UnsatisfiedLinkError`, `couldn't find`, `SoLoader`, `FATAL EXCEPTION`,
  `FATAL`, else the first `E` line, else nothing (and then the hint says the
  log carried no explaining line — never a guess). Only one line, because a
  React Native launch opens with hundreds of SoLoader lines.
* **verify route:** `Backend::launch` now returns
  `BackendError::Error { code: PlaceholderContent }` for a placeholder frame, so
  a `launch` scenario step cannot pass on a splash that never mounts.
* **Normal path untouched:** the predicate is the pre-existing
  `first_frame_is_bare_skeleton` (≤ 16 nodes **and** no app text/description/hint
  anywhere, after the whole `ACOS_ART_FIRST_FRAME_MS` budget). A content frame
  never enters this branch; `a_content_launch_stays_the_normal_pass_path`
  asserts that.

### Tests (`crates/acos-cli`, `engine::serve::tests`)

* `a_placeholder_launch_is_blocked_not_a_pass` — envelope status is `blocked`
  (not pass), code `PLACEHOLDER_CONTENT`, `details.reason =
  placeholder-content`, the hint quotes the log line, and the no-log-line form
  still names the reason.
* `the_placeholder_hint_line_prefers_the_missing_native_library` — marker order,
  `E`-line fallback, and `None` on a warning-only log.

`cargo test --release --offline -p acos-cli --bin acos` → **121 passed, 0
failed** (120 + the 1 added in this commit set; see §4 for the full command).

---

## 2. Item (1) — `screenshot` on the observe verbs

### Before

* `acos_observe_tree` had no `screenshot` argument at all.
* `acos_observe` read `screenshot` **only** in the frame-only branch; on the
  tree/text branch (the one every agent uses) it was parsed and silently
  dropped: no file, no error, no `screenshot` field. That is the reported
  "silent no-op".
* No MCP tool could produce an inline image from a live session.

### After

`acos_observe_tree` and `acos_observe` both accept

* `screenshot: true` → writes to `<session artifacts>/screenshots/<verb>-<frame_seq>.png`
  and returns the **absolute** path;
* `screenshot: "<path>"` → the CLI's `--screenshot <path>` form, honoured on the
  tree branch too;
* anything else (`7`, `""`) → `INVALID_ARGUMENT` (`the screenshot argument is
  true or a path`), never a silent no-op.

The reply's `screenshot` object:

```json
{
  "path": "/abs/.../screenshots/observe_tree-7.png",
  "bytes": 69284, "format": "png",
  "frame_seq": 7, "md5": "...",
  "width": 1080, "height": 2209,
  "display_size": [1080, 2400],
  "display_origin": [0, 191],
  "note": "lossless PNG ... png_y = display_y - display_origin.y"
}
```

* **Compressed by construction**: the bytes are the launcher's own artifact
  (SHOTZ/fdeflate encoder), copied with one `fs::copy` — never re-encoded, never
  re-inflated.
* **`display_origin`** is the session's measured display-to-window offset
  (`input_offset`, `bounds_on_screen` minus window-local `bounds`), the same
  offset the input channel applies. `display_size` is the display. `width`/
  `height` are read from the PNG's IHDR (a 24-byte header read, not a 10 MB
  decode).
* **Bound to the frame**: the tree reply's own PNG artifact is used when the
  bound traversal carried pixels; otherwise one `capture_frame()`. The
  frame-only `acos_observe` path writes from the frame it just captured (a
  second capture would have made `frame` and `screenshot` disagree).
* **Placeholder / no-pixel frame → structured `NO_FRESH_FRAME`**
  (`placeholder_screenshot_error`, detail names the placeholder and the verb; the
  launch hint points at `first_frame`/`acos_logs`), and an empty artifact file is
  also `NO_FRESH_FRAME`. Never an empty success, never splash pixels presented as
  the app.
* **MCP bounded inline preview**: `crates/acos-mcp/src/server.rs` gained
  `append_screenshot_content`, which reads `result.screenshot.path`, inlines the
  same **SHOTZ-bounded** preview an evidence image gets (long side
  `ACOS_MCP_PREVIEW_MAX_SIDE` 1280, file `ACOS_MCP_MAX_IMAGE_BYTES` 512 KiB),
  adds the `preview_note` text line naming the lossless file, and a
  `resource_link`. A path that does not exist or is not a PNG is neither inlined
  nor linked; an evidence screenshot of the same file is never inlined twice.
* `acos_observe_tree`'s MCP description documents the argument and the
  `display_origin` mapping.

### Tests

* `crates/acos-cli` `engine::serve::tests`:
  `a_screenshot_request_writes_a_png_and_reports_its_geometry` (absolute path,
  default name `observe_tree-7.png` under the session artifacts, byte-identical
  copy, `display_size`/`display_origin`, IHDR size),
  `a_screenshot_path_argument_is_honoured_and_made_absolute`,
  `the_screenshot_argument_is_true_or_a_path_and_nothing_else` (incl.
  `INVALID_ARGUMENT`), `a_placeholder_frame_has_no_screenshot_to_give`
  (`NO_FRESH_FRAME`).
* `crates/acos-mcp` `server::tests`:
  `a_result_screenshot_is_inlined_as_a_bounded_preview` (image block + lossless
  line + resource_link; no `screenshot` → no extra content; missing file → no
  inline, no link).

---

## 3. Item (3) — `acos_inspect` native-lib cross-check — **NOT STARTED**

Nothing done. The smallest next action is in §6.

---

## 4. Tests actually run

`cargo test --release --offline -p acos-cli -p acos-art -p acos-mcp
-p acos-verify -p acos-protocol`, through
`tools/art-runtime/run-exclusive.sh --task FB2 -- tools/guarded.sh --mem 10G
--time 900 --`, `CARGO_TARGET_DIR=.cache/gr4-target`:

```text
53 "test result: ok" suites; 0 failures; 0 `FAILED`; 0 `panicked`
```

(acos-cli lib/bin 120→121 tests, acos-mcp 60, acos-art 121+27, acos-verify,
acos-protocol incl. `schemas_are_up_to_date` after `UPDATE_SCHEMA=1`.)

Not run: the 15 gates, the 5 Gaten flows + 5 negatives, the crafted-APK
acceptance, the live MCP smoke.

## 5. Decisions

* **A new error code, not a reused one.** No existing code maps to `blocked`
  and means "the window exists, the app's content never mounted"; reusing
  `NO_FRESH_FRAME` (`Error`, infra-timeout class) would have put the case in the
  wrong class. `PLACEHOLDER_CONTENT` is additive and schema-regenerated.
* **`stage_reached: "window"` in the blocked result**, with the `first_frame`
  ladder rung rewritten to `blocked`. The alternative (keep `first_frame` and
  only change the status) would have left an agent reading `stage_reached` the
  same string it reads on a pass.
* **One log line, chosen by marker order**, not a log tail: the agent complaint
  was 235 lines of SoLoader noise before anything app-specific.
* **Screenshot refusal is `NO_FRESH_FRAME`** (the code the feedback named), not
  `PLACEHOLDER_CONTENT`, because the *verb* asked for pixels and there are no app
  pixels to give; the detail names the placeholder so the two are linked.
* **`screenshot: true` writes under the session artifacts**, not the caller's
  cwd: the MCP server's cwd is not the agent's, and a relative path that lands
  somewhere unreachable was one of the reported traps.
* **`acos verify` is affected too** (a placeholder launch step is now an error,
  not a pass). That is intended: same false green, one layer down. It is why the
  gates must be re-run before this is called done.

## 6. Remaining + smallest next action

1. **Run the acceptance suite** (needs the ART lock, ~30-40 min):
   `tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh` around the gate
   runner, expecting 10 pass / 3 `ASSERTION_FAILED` / 1 `ELEMENT_OFFSCREEN` /
   1 `TIMEOUT`, then the 5 Gaten flows + 5 negatives. The risk to watch: a Gaten
   launch that lands on a placeholder would now fail the launch step where it
   previously passed — that would be the *right* verdict but it must be
   confirmed that Gaten is not affected in practice (243-node content frame, so
   it should not be).
2. **MCP smoke on Gaten**: `acos_observe_tree {"screenshot": true}` → a PNG
   < 1 MB on disk that decodes, `display_origin` present, inline preview in the
   reply; and the crafted APK (one `lib/x86_64/lib*.so` removed, re-signed with
   the fixture debug key, scratch under `.cache/fb2/`) → launch `blocked` /
   `PLACEHOLDER_CONTENT`, `acos_inspect` warns the library is missing.
3. **Item (3)**: `acos_inspect` — cross-check the library names the APK
   references (`System.loadLibrary` / `SoLoader.loadLibrary` string constants in
   the dex, `libappmodules`/`libworklets`-style RN names) against the APK's
   `lib/<abi>/` entries and add a `warnings` array to the inspect reply. Two
   tests: a crafted copy missing one library warns exactly that one; the stock
   APK warns nothing (0 false alarms).
