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

**Status: DONE (FB2b).** Items (1) and (2) landed in FB2 (§1, §2 below); FB2b
adds the APK-level acceptance (§7: 15 gates, 5 Gaten flows + 5 negatives), the
live MCP smoke on Gaten (§8), the crafted-APK proof (§9) and item (3), the
`acos_inspect` native-lib cross-check (§10). Nothing here weakens an assertion,
a gate or a status: every change makes a non-pass more likely, never a pass.

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

## 6. FB2b — the APK-level acceptance, run for real

Everything below ran on the release binaries from `.cache/gr4-target` with the
main checkout's lock: `tools/art-runtime/run-exclusive.sh --task FB2b-* --
tools/guarded.sh --mem 10G --time 900 -- <cmd>`. Raw evidence:
`results/fb2b/sweep.log`, `results/fb2b/gates-summary.tsv`,
`results/fb2b/gaten-summary.tsv`, `.cache/fb2/*.jsonl`.

### 6.1 The 15 gates (A)

Runner `.cache/fb2/run_gates.sh` (one binary, sequentially,
`acos verify <scenario> --engine art --artifacts .cache/fb2/gates/artifacts/<stem>`).
**15/15 as specified — 0 false passes, 0 false failures.**

| gate | expected | observed | code |
|---|---|---|---|
| `v2-art-counter` | pass | pass | - |
| `w22-counter-reset` | pass | pass | - |
| `w54-dialog-pixels` | pass | pass | - |
| `w42-back-frame` | pass | pass | - |
| `w15-dialog-menu` | pass | pass | - |
| `form-validation` | pass | pass | - |
| `suite/T03-render-good` | pass | pass | - |
| `v1-offscreen-pager` | pass | pass | - |
| `w7-insets-report` | pass | pass | - |
| `se1-expect-change-tap` | pass | pass | - |
| `suite/T03-render-wrong` | fail | fail | `ASSERTION_FAILED` |
| `w7-insets-carve-negative` | fail | fail | `ASSERTION_FAILED` |
| `negative/se1-expect-change-noop` | fail | fail | `ASSERTION_FAILED` |
| `negative/offscreen-pager-visible` | fail | fail | `ELEMENT_OFFSCREEN` |
| `p11-progress-timeout` | timeout | timeout | `TIMEOUT` |

**No launch landed on `PLACEHOLDER_CONTENT`**: `grep -l PLACEHOLDER_CONTENT` over
all 15 reply JSONs is empty, so the detector did not need a fix and no gate was
weakened. (The new code would have caught it: every gate's launch ladder still
ends `first_frame` with `ok: true`.)

### 6.2 The 5 Gaten flows + 5 negatives (A)

Runner `.cache/fb2/run_gaten.sh`, APK
`/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`
(the one the gaten gates name), read-only.

`gaten-home`, `gaten-tabs`, `gaten-settings`, `gaten-search`, `gaten-lesson` →
**pass** (7/10/12/11/12 s). `scenarios/negative/gaten-{home,tabs,settings,
search,lesson}-wrong` → **fail `ASSERTION_FAILED`** (5/5, 5-8 s each). 10/10,
0 false passes.

## 7. (B) Live MCP smoke — a screenshot through the observe verb

`tools/release/mcp_smoke.py` gained `--mode gaten-shot`: launch the APK, call
`acos_observe_tree {"screenshot": true}`, then assert the PNG on disk and the
reply's inline preview. Driven from an **empty directory outside any checkout**
(`.cache/fb2/outside`), **defaults** (no `ACOS_*` env), against the scratch
install `tools/release/install.sh --from-target .cache/gr4-target --prefix
.cache/fb2/prefix`. Evidence: `.cache/fb2/mcp-gaten-shot.jsonl` (every JSON-RPC
frame + one `{"check":...}` line per assertion).

```text
acos_launch                       pass   engine=art stage=first_frame
observe_tree_screenshot_status    pass
screenshot_path_absolute_and_exists pass /run/user/1000/acos/art-sessions/mcp-3418842/screenshots/observe_tree-3.png
screenshot_bytes_under_1MB        pass   138 839 bytes (< 1 MiB)
screenshot_is_a_png               pass   magic 89504e470d0a1a0a, IHDR 1080x2400
screenshot_geometry               pass   display_size [1080,2400], display_origin [0,0], IHDR == reported
screenshot_inline_preview_present  pass   one image/png content item
screenshot_inline_preview_bounded  pass   80 394 bytes (<= 512 KiB), 576x1280 (<= 1280 long side)
screenshot_preview_names_the_lossless_file pass  "screenshot: 576x1280 downscaled preview of 1080x2400; lossless file: <path> (138839 bytes)"
acos_destroy / status_after_destroy pass   SESSION_NOT_FOUND
smoke: ok
```

### 7.1 A discovery bug this exposed (fixed)

The first run returned `status: pass` with **no** `screenshot` field. The cause
was not the observe verb: `acos-mcp` resolves its CLI backend with
`ACOS_BIN -> acos on PATH -> target/{release,debug}/acos`, so the scratch-prefix
server silently drove the **`~/.local/bin/acos`** of another installation
(`acos backend: /home/Qui/.local/bin/acos` in its stderr log), a build without
the FB2 screenshot code. Fixed in `crates/acos-mcp/src/backend.rs`
(`discover_binary`): after `ACOS_BIN`, prefer **the `acos` installed beside the
running executable** (the REL1a layout), then `$ACOS_HOME/bin/acos`, then
`PATH`, then the repo walk. Unit test
`the_installed_cli_beside_this_binary_wins_over_path`. An agent that installs
into a prefix and runs from anywhere now drives *that* install, not whatever
`acos` PATH happens to hold.

## 8. (C) Crafted APK — a launch that cannot mount is `blocked`

Craft (scratch `.cache/fb2/crafted/`, script `.cache/fb2/crafted_check.py`):
copy the stock Gaten APK, rewrite the zip **without**
`lib/x86_64/libworklets.so`, `zipalign -f 4`, then `apksigner sign` with the
fixture debug key exactly as `tools/build-apps.sh` does
(`fixtures/build/apps/debug.keystore`, alias `androiddebugkey`, pass `android`,
v1+v2+v3). `apksigner verify --print-certs` → `CN=Android Debug, O=Android,
C=US`. `libreanimated.so` and `libgesturehandler.so` both carry
`DT_NEEDED libworklets.so`, so the removal is a real dangling dependency.

Over MCP stdio (empty dir, scratch prefix, `.cache/fb2/crafted.jsonl`):

| check | result |
|---|---|
| `acos_inspect` on the crafted APK | `warnings: [{code: NATIVE_LIB_MISSING, lib: libworklets.so, abi: x86_64, source: elf-needed, referenced_by: [libgesturehandler.so, libreanimated.so]}]` — exactly one, the removed library |
| `acos_launch` on the crafted APK | `status: blocked`, `error.code: PLACEHOLDER_CONTENT`, `result.stage_reached: window`, `result.content: {mounted: false, reason: placeholder-content}`, `details: {nodes: 9, traversals: 8, waited_ms: 267, event_waits: 7, frame_number: 1}` |
| the hint | quotes the guest log line: `... java.lang.UnsatisfiedLinkError: No implementation found for boolean ai.onnxruntime.telemetry.HttpClient.isDeviceIdCollectionEnabled() ... is the library loaded, e.g. System.loadLibrary?` |
| `acos_verify` of `gaten-home` on the crafted APK | **not a pass**: `PLACEHOLDER_CONTENT` ("launch reached the window skeleton but the app's content never mounted (9 node(s), no app text) after 8 traversal(s)/267 ms") |

Before FB2 this APK was a green `pass` / `first_frame` with an empty RN surface.

## 9. (D) `acos_inspect` cross-checks the native libraries

New module `crates/acos-apk/src/native.rs` (no new dependency, no `unsafe`):

* `elf_needed(bytes)` — `DT_NEEDED` of an ELF32/ELF64 **little-endian** object,
  bounds-checked, never panics on a truncated/hostile file;
* `dex_load_library_names(bytes)` — the **call sites**: a
  `const-string`/`jumbo` string index is followed within 16 bytes by an
  `invoke-static`/`invoke-static/range` whose `method_id` is named
  `loadLibrary` on a class whose descriptor names `System` or `SoLoader`
  (`System.loadLibrary`, `SoLoader.loadLibrary`). A bare string that merely
  looks like a library is *not* a reference — that is what keeps the stock APK
  at zero warnings;
* `missing_native_libs(apk, abi)` — the two sources above minus what
  `lib/<abi>/` contains minus the documented platform set `SYSTEM_LIBS`
  (`libc.so`, `libm.so`, `libdl.so`, `liblog.so`, `libandroid.so`,
  `libjnigraphics.so`, `libEGL/libGLESv2`, `libz.so`, …24 entries), each warning
  `{code: NATIVE_LIB_MISSING, lib, abi, source, referenced_by}`.

`acos inspect` reports it as `result.native_lib_check = {abi, warnings,
platform_libs_ignored}` for the ABI ACOS would run (host `x86_64` when present,
else the APK's first ABI), and prints one `warning:` line per entry (a healthy
APK prints `native lib check (x86_64): all referenced libraries present`).
`acos_inspect` forwards it unchanged (the tool runs `acos inspect --json`).

**Real APK checks (0 false alarms):**

| APK | abi | warnings |
|---|---|---|
| stock Gaten `gaten-dev-f344272-x86_64-release-unsigned.apk` | x86_64 | `[]` (0) |
| crafted `gaten-no-worklets.apk` (lib/x86_64/libworklets.so removed, re-signed) | x86_64 | exactly `libworklets.so` (`source: elf-needed`, `referenced_by: [libgesturehandler.so, libreanimated.so]`) |

Cost: `acos inspect` on the 114 MB Gaten APK is 0.23 s wall.

**Unit tests** (`cargo test -p acos-apk`, 29 lib tests): `tiny_elf` DT_NEEDED
parsing (64-bit, plus truncation/non-ELF/empty), a hand-built DEX whose
`SoLoader.loadLibrary("worklets")` call site yields `libworklets.so` (and a
`libfoo.so` argument is normalised), a synthetic APK **with** the library →
0 warnings and the same APK **without** it → exactly `libworklets.so` from both
sources with `libc.so` never warned, plus name/`SYSTEM_LIBS` guards. **Real APK
checks** are the two rows above (both run, output in §9 and §8).

## 10. Docs

* `docs/AGENT_GUIDE.md`: workflow step 2 (inspect's `native_lib_check` +
  `NATIVE_LIB_MISSING`), step 3 (`PLACEHOLDER_CONTENT` as a blocked launch, what
  `log_hint` and `content.mounted: false` mean, "do not retry with a longer
  timeout"), the errors table row for `PLACEHOLDER_CONTENT`, and the scenarios
  topic's screenshot section (`acos_observe_tree {"screenshot": true}`, absolute
  path, `display_origin`, `NO_FRESH_FRAME` on a placeholder).
* `docs/MCP.md`: the `screenshot` argument on `acos_observe_tree`/`acos_observe`
  (including the `display_origin` mapping), a "a launch that never mounts is
  `blocked`" section, and the `acos_inspect` `native_lib_check` shape with its
  two reference sources; both tools' rows in the tool table now name the
  argument.

## 11. Tests actually run

`cargo test --release --offline -p acos-cli -p acos-art -p acos-mcp
-p acos-verify -p acos-protocol -p acos-apk`, `CARGO_TARGET_DIR=.cache/gr4-target`,
through the lock and `tools/guarded.sh --mem 10G --time 900`:

```text
60 "test result: ok" suites; 655 tests passed; 0 failed; 0 FAILED
(results/fb2b/tests.log)
```

Notable suites: acos-cli 121 bin + 120 lib, acos-art 121 lib + the integration
suites, acos-mcp 61 lib + the docs/metadata suites (the generated tool table in
`docs/MCP.md` is compared byte for byte — see below), acos-verify, acos-protocol
(incl. `schemas_are_up_to_date`; no schema changed in FB2b), acos-apk 29 lib
tests including the four new `native::tests`.

Three runs were needed, and what failed in the earlier ones is worth recording
because neither was caused by this branch's logic:

1. `acos-apk` `native::tests::elf_needed_reads_a_minimal_dynamic_section` — my
   **test fixture** was off by one byte in the fake string table (it asserted
   `libc.so` at offset 15 where it lives at 16, so the parser correctly returned
   the empty string at 15). Fixture fixed; the parser was right.
2. `acos-art` `launch_hang::guest_killed_by_signal_returns_signal_and_cleans_up`
   — `launch took 11.428 s ... (bound: 5 s)` while three other agents were
   compiling and benching on this host. This is the known **LHFLAKE**
   (`results/handoff/SHOTZ.md` §5/§6: the `launch_hang` family fails only when
   the whole suite runs under host load and passes in isolation, recorded on
   `main` as well). It **passed** unchanged in the next full run (15.02 s suite,
   7/7) — the test was not touched, skipped or loosened.
3. `acos-mcp` `docs::docs_tool_table_matches_descriptors` — a real finding about
   **my** doc edit: the table between the markers is generated from the tool
   descriptors, so adding "(`screenshot` optional)" to two cells drifted it. The
   table is back to the generated form and the `screenshot` argument is
   documented in the prose section instead (regenerating with
   `cargo run -p acos-mcp --example gen-tools-docs` would also drop such edits
   silently, so the prose is the honest place for it).

## 12. Remaining + smallest next action

Nothing in FB2's scope is left open. See §13 for the run log and §14 for the
decisions; the two follow-ups that are **not** FB2's to do are listed there.

## 13. FB2b run log (chronological, for a re-audit)

```text
01:26  gates, first pass       15 runs, 14 OK / 1 FAIL -- p11-progress-timeout
                               reported status=timeout, my runner expected "fail"
                               (runner bug, gate correct: the code was TIMEOUT)
01:27  gates, second pass      OK=15 FAIL=0 (results/fb2b/gates-summary.tsv)
01:29  gaten sweep             OK=10 FAIL=0 (flows pass, negatives ASSERTION_FAILED)
01:41  crafted APK             stripped + zipalign + apksigner (debug key)
01:45  MCP smoke (B)           smoke: ok, PNG 138 839 B, preview 576x1280
01:45  crafted check (C)       blocked/PLACEHOLDER_CONTENT, inspect warns 1 lib,
                               verify gaten-home not a pass
02:02  tests                  60 suites, 655 passed / 0 failed (results/fb2b/tests.log)
02:05  cleanup                 scratch prefix (.cache/fb2/prefix) and the crafted
                               APK deleted; .cache/fb2 keeps only small logs
```

## 14. Decisions for the orchestrator

* **`acos-mcp` backend discovery now prefers the sibling `acos`** (then
  `ACOS_HOME`, then `PATH`). This is a behaviour change beyond "screenshot in
  observe": it is the bug that made a fresh install worthless when any other
  `acos` is on `PATH`. If the release layout ever moves `acos` out of
  `<prefix>/bin`, `ACOS_BIN` is still the escape hatch.
* **`NATIVE_LIB_MISSING` is a warning, not an error and not a verdict.** An APK
  that references a system-only library outside the 24-entry `SYSTEM_LIBS` set
  would warn spuriously; the set is documented in `acos_apk::native` and is the
  only place that needs extending if that ever happens. The stock Gaten APK
  (23 bundled libs) is the regression check: 0 warnings.
* **`acos verify` is affected too** (a placeholder launch step is now an error,
  not a pass) — intended, same false green one layer down, and the Gaten sweep
  above proves the pass paths are untouched.
* **`PLACEHOLDER_CONTENT` as a `blocked` status, not a new rung**: FB2 §5.

---

## Review (Opus review of FB2 / FB2b — copied from the FB2c brief)

1. **HIGH** `first_frame_is_bare_skeleton` (engine/art.rs) blocked on node count
   alone: a full-screen custom View / SurfaceView / GL / image-only activity /
   Flutter without semantics / RN ActivityIndicator-only screen was wrongly
   `PLACEHOLDER_CONTENT`. Needs evidence beyond the tree (frame not advancing /
   placeholder pixels); a skeleton with real pixels = pass + `placeholder_suspect`.
2. **HIGH** an unsettled first traversal returned `one_traversal` with the
   bare-skeleton verdict and `waited_ms: 0`: the content wait
   (`ACOS_ART_FIRST_FRAME_MS`) never ran. The error must name the budget and the
   knob; docs/MCP.md "Never retry with a longer timeout" is wrong for slow RN
   bundles; "frame_number N did not advance" was asserted, not measured.
3. **MED-HIGH** `reset` skipped `bind_first_frame` (and `acos_reset` hardcoded
   `stage_reached: first_frame`): a relaunch that never mounts passed.
4. **MED** the placeholder state was sticky (`first_frame_report` set once).
5. **MED** screenshot path not confined (serve.rs; MCP `append_screenshot_content`):
   any path, `..`, symlink, dest == source (truncates the frame).
6. **LOW-MED** `screenshot_source` could capture a newer frame than the tree in
   the same reply.
7. **LOW-MED** acos-mcp discovery order: ACOS_BIN > $ACOS_HOME/bin/acos >
   sibling of current_exe > PATH > repo walk, plus a stderr warning on a version
   mismatch.
8. **LOW** `acos_inspect` loadLibrary scan was a raw byte scan; ignore
   `jscexecutor` when Hermes ships; SYSTEM_LIBS += libaaudio/libamidi/
   libdl_android; the guide must call the warning a hint.
9. cosmetic: runs of spaces in the `Backend::launch` detail string.

## FB2c — what was fixed

A previous agent ran out of quota mid-fix and left ~1090 uncommitted lines; this
continuation read all of it, kept what was right and finished the rest.

| # | Fix | Where | Tests |
|---|---|---|---|
| 1 | verdict = tree skeleton **and** no frame movement (no newer `frame_seq`/`frame_number`, identical pixel digest; pixels decide when comparable). Skeleton + moved frame = pass, `result.first_frame.placeholder_suspect: true`, `warnings[PLACEHOLDER_SUSPECT]`, `frame.meaningful` stays true | `engine/art.rs` `first_frame_placeholder_verdict`, `serve.rs launch_reply` | `the_placeholder_verdict_needs_frame_evidence_not_a_node_count`, `fb2c_a_skeleton_with_changing_pixels_passes_with_a_suspect_warning` |
| 2 | the unsettled first traversal is the rollback baseline, not an early return: the content wait always runs first; error/hint name `ACOS_ART_FIRST_FRAME_MS` and the spent budget; the "frame_number" sentence is built from measured facts only (`placeholder_evidence_sentence`); docs/MCP.md + AGENT_GUIDE rewritten (budget is for slow RN bundles; a longer timeout after a *spent* budget cannot help) | `art.rs bind_first_frame`, `serve.rs placeholder_launch_error`, docs | `fb2c_an_unsettled_first_traversal_still_spends_the_content_budget`, `placeholder_launch_error` test (budget, measured number, unmeasured number never "did not advance") |
| 3 | `ArtSession::reset` binds the relaunched first frame through `bind_first_frame`; a placeholder is `PLACEHOLDER_CONTENT` (serve + verify `reset` step); the reset reply carries `first_frame` | `art.rs reset`, `serve.rs reset` | `fb2c_a_reset_that_never_mounts_is_blocked` |
| 4 | not sticky: `absorb` clears the report when a later frame has content; `placeholder_frame_report()` answers from the current frame | `art.rs` | `fb2c_a_placeholder_is_cleared_once_a_later_frame_has_content` |
| 5 | `confine_screenshot_dest`: only `true` or a path inside the session artifacts dir (relative = relative to it; canonicalized; no `..`; no symlink escape, nothing created through a link; dest != source canonical); MCP inlines only files under its allowed roots (`path_within_allowed_roots`, canonical) | `serve.rs`, `acos-mcp/src/server.rs` | `a_screenshot_destination_cannot_leave_the_session_artifacts`, `a_screenshot_is_inlined_only_from_an_allowed_root` |
| 6 | `screenshot_source` returns only the bound tree's own PNG; a tree without pixels is refused (`NO_FRESH_FRAME`) instead of being paired with a newer capture, so `screenshot.frame_seq == tree frame_seq` | `art.rs` | `fb2c_a_screenshot_never_comes_from_a_newer_frame_than_the_tree` |
| 7 | discovery order `resolve_binary_from` (ACOS_BIN > ACOS_HOME > sibling > PATH > repo walk — identical to main v0.2.1) + one stderr warning on `--version` mismatch / unanswered | `acos-mcp/src/backend.rs` | `discovery_order_is_bin_then_home_then_sibling_then_path`, `a_cli_version_mismatch_is_detected` |
| 8 | `loadLibrary` call sites are **decoded** (class_defs → class_data → code_item → `acos_dex::decode`, `const-string` register tracking paired with the `invoke-static`), no byte scan; `jscexecutor` ignored when `libhermes.so` ships; SYSTEM_LIBS += libaaudio/libamidi/libdl_android; AGENT_GUIDE calls it a hint | `acos-apk/src/native.rs` (+ `acos-dex` dep) | `a_call_site_is_a_decoded_instruction_not_a_byte_pattern`, `an_operand_byte_that_looks_like_an_opcode_is_not_an_instruction`, `jscexecutor_is_ignored_when_hermes_ships_and_audio_libs_are_system` |
| 9 | the Backend::launch detail string is built by `placeholder_evidence_detail` (no embedded runs of spaces) | `art.rs` | covered by the item-3 test |

Found while running the live acceptance (and fixed):

* item 6, the first cut (refuse a tree without pixels) broke `acos_observe_tree
  screenshot:true`, because the tree traversal was tree-only. A request with a
  screenshot now asks the **same traversal** to capture (`tree_with_settle_pixels`),
  so `screenshot.frame_seq == tree.frame_seq` by construction.
* item 5, the first cut (MCP inlines only under `allowed_roots`) stopped inlining
  the default screenshot (`<runtime>/acos/art-sessions/<id>/screenshots/`). That
  tree (and `ACOS_SERVE_ARTIFACTS`) is now a root of its own; nothing else
  outside `allowed_roots` is inlined.
* the placeholder evidence compares the baseline against the **latest** traversal
  (taken before the rollback), so an animating skeleton that never settles is not
  "unchanged" by construction. A launch whose retry saw app content but never
  settled is now `placeholder_suspect` (pass, skeleton bound), no longer blocked
  (`g3f1_unsettled_retry_rolls_back_to_the_settled_traversal` updated).
* test fakes: the reset fakes returned an empty tree (0 nodes), which is a
  skeleton; they now carry a text node (a mounted app). `g3f1_content_first…`
  asserted `waited_ms == 0` for one forked-shell traversal (a 1 ms flake); it now
  asserts `< 250` and keeps the request-count + `event_waits == 0` proofs.

### FB2c acceptance (A: gr4 build, scratch prefix `.cache/fb2c/prefix`, defaults, empty dir, `env -u ACOS_HOME`)

| criterion | result |
|---|---|
| unit tests per finding | see table above; all green |
| crafted Gaten APK without `lib/x86_64/libworklets.so` (zipalign + apksigner, debug keystore, v1+v2+v3) over MCP | `.cache/fb2c/crafted.jsonl`: inspect warns exactly `libworklets.so`; **launch → blocked PLACEHOLDER_CONTENT** (message: `8 frame(s) over 8 traversal(s)/207 ms (7 event wait(s), budget ACOS_ART_FIRST_FRAME_MS=5000 ms): frame_number advanced to 1, the pixels are identical to the skeleton's`); hint names the log line; **`acos_reset` on the blocked session → blocked PLACEHOLDER_CONTENT**; `acos_verify` of gaten-home → blocked; `smoke: ok` |
| normal counter + Gaten pass | gates `v2-art-counter` etc. below; `mcp_smoke --mode counter` ok; Gaten 5 flows ok |
| 15 gates exact | `.cache/fb2c/gates/summary.tsv`: OK=15 FAIL=0 (10 pass; T03-render-wrong, w7-insets-carve-negative, se1-expect-change-noop → ASSERTION_FAILED; offscreen-pager-visible → ELEMENT_OFFSCREEN; p11-progress-timeout → TIMEOUT) |
| Gaten 5+5 | `.cache/fb2c/gatensweep/summary.tsv`: OK=10 FAIL=0 (5 pass, 5 `*-wrong` ASSERTION_FAILED) |
| `mcp_smoke.py --mode gaten-shot` from a scratch install | `smoke: ok` (inline image present, frame_seq equal) |
| `cargo test --release --offline -p acos-cli -p acos-art -p acos-mcp -p acos-verify -p acos-protocol -p acos-apk` | green, 60 `test result: ok` blocks, 0 failed (`--no-fail-fast`; `acos-art launch_hang::guest_exits_before_milestone…` flaked once under benchmark load — 13.6 s vs 5 s bound — and passed on the rerun; that crate is untouched) |

Remaining: nothing known. Decision for the orchestrator: a skeleton whose
pixels changed is now a **pass** with `placeholder_suspect` (warning
`PLACEHOLDER_SUSPECT`), by design (finding 1); a text-free screen can therefore
no longer be proven "unmounted" by the launcher alone.
