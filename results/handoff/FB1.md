# FB1 — live-session typing and explicit `content_desc` selectors

**Worktree:** `/home/Qui/.paseo/worktrees/3oxa6xao/s3ab`
**Branch:** `fb1-live-input` (from `rn-tap-visibility` @ `5065e82`, merged `main` @ `74e4ee9` = v0.2.0)
**Track:** FB1 (owner priority: agent feedback, items 2 and 3 of the subagent log + Triage)
Not pushed, not merged. **Status: DONE** (commits `dce3b14`, `1f53bde`; see §4 and §6).

---

## 1. What an agent hit (the two items)

Feedback item **2** (P1): *"No typing on a live session"*. Focus the search field,
then `acos type --text kayou` → `UNSUPPORTED_BACKEND`, "does not provide
`text.input` yet", while the scenario `type` step types into the same APK fine.

Feedback item **3** (P1): *"`tap --text` ignores `content_desc` and there is no
`--content-desc`"*. `acos tap --text "Search Japanese dictionary"` →
`ELEMENT_NOT_FOUND`; the label lives in the content description, so the agent had
to read bounds and hand-compute a centre point.

## 2. Root cause (measured, not guessed)

Both are live-channel defects; the scenario path was already correct.

**Typing.** `serve.rs`'s `acos_text` refused *before* injecting whenever the tree
had no node with `focused: true` **and** a class ending in `EditText`
(`focused_editable()`). Gaten's search field is an RN `TextInput`: the guest
reports its class obfuscated (`Z4.g` in the API-34 tree `/tmp`-free dump
`.cache/fb1/live1/tree-home.json`), so the gate could never pass — even with the
field focused and the cursor in it. The scenario `type` step never asks that
question: it sends the guest's own `type_text` command
(`ArtSession::type_text` → `{"cmd":"type_text","text":…}` →
`InputHost::type_text` → the real keyboard/`InputConnection` path) and judges the
settled result. Proof of the old refusal, with the field focused by bounds:
`.cache/fb1/live1/type.json` (UNSUPPORTED_BACKEND, `text.input`).

**Selectors.** The live verbs accepted only `id`, `text` and `x`/`y`
(`serve.rs::tap`), and their `text` resolution used the access-name rule, whose
second stage requires the matched node's own `text` to be empty. An RN
`TextInput` reports its **placeholder as its text** (AOSP
`TextView.getTextForAccessibility()` falls back to the hint; the probe copies it
and the hint stays a separate field), so
`text="Search Japanese dictionary"` reaches neither stage: the node's text is
`猫, ねこ, neko...` and its name is the content description. There was no way to
say "match the content description".

## 3. What changed

### 3.1 Typing = the scenario path, on the live channel

* `crates/acos-cli/src/engine/serve.rs` — `acos_text` no longer gates on an
  `EditText` suffix or on the probe's `focused` flag. It does exactly what the
  executor's `type` step does: `session.type_text(text)` (the guest's
  `type_text` command), then the requested settle decides what changed. The
  reply is the same `input_result` shape (action/accepted/state_changed/acks/
  target/settle).
* Any genuine wall still reports itself honestly: a guest that cannot accept the
  keys answers `INPUT_UNAVAILABLE`, a settle that never completes is `TIMEOUT`,
  and no error is turned into a pass.

### 3.2 The reply names the field; the echo is masked unless the field is known to be plain

* `crates/acos-protocol/src/ui.rs` -- `UiNode.password`, parsed from the probe's
  `password` key (`AccessibilityNodeInfo.isPassword()`); serialized in
  `acos_observe_tree` so an agent can see which field is a secret (`password:
  true`). The tree text is left as the platform reports it.
* `serve.rs::focused_input_node` / `echo_decision` / `input_target` (final rule,
  commit `1f53bde`, after the orchestrator's ruling):
  - `result.target.node` is the **visible focused node** the input went to, as the
    tree shows it. No other node is ever substituted.
  - The typed text is echoed (`target.text`) only when that focused node is **not** a
    password field.
  - Otherwise the reply masks: `target.text: "*"`, `text_masked: true`, `chars` (the
    honest count) and a `reason`. The reasons are: a focused password field (the
    node's own text is withheld too); `mask: true`; or **target unknown** (the tree
    names no focused field -- `node` is `null`, nothing is guessed).
* `mask: true` on `acos_text` forces masking. `mask` must be a boolean.
* An earlier heuristic ("mask the single password node when nothing is focused")
  named an unrelated credential field as the target in the Gaten smoke. It was
  removed. The superseded state is commit `dce3b14`.

### 3.3 `content_desc` selectors on the live channel (+ no silent picks)

* `serve.rs::args_selector` — `acos_tap` / `acos_assert` / `acos_action`'s inner
  verbs accept `content_desc` (exact) alongside `id`/`text`, plus `index`.
  Precedence: `id` > `content_desc` > `text`.
* `serve.rs::resolve_input_node` → `UiTree::find_input_candidates` (new,
  protocol): the RNTAP accessible-name rule, but it returns **every** match of
  the winning stage. One match is the target; zero keeps the established ladder
  (`ELEMENT_OFFSCREEN` when only an invisible node matches, `ELEMENT_NOT_FOUND`,
  `TREE_TRUNCATED`); **several** are refused with the new
  `AMBIGUOUS_SELECTOR` error whose `details.candidates` lists id, resource id,
  text, content_desc, class and bounds of each match. Nothing is injected on a
  guess.
* Assertions stay **field-exact**: `acos_assert {"text": …}` never falls back to
  a content description (RNTAP's rule is an *input* rule), and an ambiguous
  assertion is refused the same way instead of silently asserting the first
  match.
* `crates/acos-cli/src/session.rs` — `acos tap --content-desc <value>` and
  `acos session tap --content-desc <value>`; without a live daemon the selector
  is refused with a structured explanation instead of being ignored.
* `crates/acos-mcp/src/descriptors.rs` — `acos_tap` schema/description document
  `content_desc`, `index` and `AMBIGUOUS_SELECTOR`; `acos_text`'s description
  states parity with the scenario step, the RN class fact and the masking.
* Scenario selectors keep their documented rule (first match in document order,
  `index` picks another): a scenario is a fixed document, and the 15 gates /
  Gaten flows depend on that. The difference is stated in `docs/AGENT_GUIDE.md`.

### 3.4 Docs

`docs/AGENT_GUIDE.md`: the `workflow` step 5 (tap/text), a new recipe **"type
into a search field"** (observe → tap by `content_desc` → `acos_text` → re-observe;
why `text` cannot name an RN field; masking), the CLI reference
(`acos tap --content-desc`), the selectors section (scenario first-match vs live
ambiguity rule) and the error table (`AMBIGUOUS_SELECTOR`).
`docs/ERROR_CODES.md`: the new code with its details shape and class.

---

## 4. Acceptance evidence

Every ART run and every cargo test of `acos-cli`/`acos-art`/`acos-mcp` below went
through
`/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task FB1 -- tools/guarded.sh …`.

### A. Unit / integration tests (guest-free, no ART)

`crates/acos-cli/tests/fb1_live_input.rs` (new) starts a real `acos serve` on a
private socket with a fake `acos-art` (`ACOS_ART_BIN`) and drives the daemon
exactly as an agent does:

| test | asserts |
|---|---|
| `live_text_reaches_the_guest_type_text_command` | tap by `content_desc` focuses the RN field (`focused: true`, text = placeholder); `acos_text {"text":"kayou"}` is `pass`/`action:"text"`, and the **guest received exactly one `{"cmd":"type_text","text":"kayou"}`** with the settle mode — i.e. the same command a scenario `type` step sends |
| `ambiguous_selector_is_refused_with_candidates` | two visible `Continue` nodes → `AMBIGUOUS_SELECTOR`, `details.count 2`, both candidates listed with bounds, **nothing reached the guest**; `index: 1` picks one; `acos_assert {"text": "Search Japanese dictionary"}` is `ELEMENT_NOT_FOUND` (field-exact) while `content_desc` asserts |
| `ambiguous_assertion_is_refused_with_candidates` | an assertion naming two nodes is refused, candidates included |
| `password_typing_is_masked_in_the_reply` | focused password field → `target.text_masked: true`, `chars: 6`, `text: "*"`, and the reply carries no `s3cret` |

Unit tests:
* `acos-protocol`: `ui::tests::content_desc_is_an_explicit_selector_field`,
  `several_visible_matches_are_all_returned_as_candidates`,
  `the_text_stage_wins_over_the_content_desc_stage` (+ the RNTAP tests still green).
* `acos-cli` (`--bin acos`): `engine::serve::tests::live_selectors_prefer_id_then_content_desc_then_text`,
  `several_live_matches_are_refused_with_their_candidates`,
  `typed_text_is_masked_only_for_a_password_target`.

### B. MCP, scratch install, outside the checkout, default settings

Install: `tools/release/install.sh --from-target WT/.cache/s3ab-target
--prefix WT/.cache/fb1/prefix` (0.2.0), run from
`/home/Qui/Projects/acos-fb1-outside` under `env -i` (HOME, USER, LANG, PATH,
XDG_RUNTIME_DIR, DBUS_SESSION_BUS_ADDRESS, `ACOS_ALLOWED_ROOTS=/home/Qui`,
`ACOS_ARTIFACTS_DIR=<outside>/artifacts`) — no `ACOS_BIN`, no
`ACOS_SESSION_DAEMON`, no engine argument. Transcripts:
`.cache/fb1/mcp/{counter,gaten-search,gaten}.jsonl`.

Results (final code, commit `1f53bde`, installed prefix at `.cache/fb1/prefix`, 0.2.0):

* **counter** (`results/fb1-evidence/counter-smoke.jsonl`): `acos-mcp` from the
  scratch prefix, run from an outside directory: launch (ART, first_frame), observe
  `0`, tap Increment -> `1`, reset -> `0`, destroy (teardown verified,
  `status_after_destroy` = `SESSION_NOT_FOUND`). `{"smoke": "ok"}`.
* **gaten-search** (`results/fb1-evidence/gaten-search-smoke.jsonl`), smoke
  `tools/release/mcp_smoke.py --mode gaten-search`, Gaten `f344272` unsigned APK,
  `{"smoke": "ok"}`. In order, each a checked claim:
  1. onboarding walked by text (`Set up Gaten`, `Skip`, `Skip`); Home proven with no
     onboarding control left and the tab bar + hints present;
  2. the search field is named by its `content_desc` `Search Japanese dictionary`
     (its `text` is the placeholder `猫, ねこ, neko...`);
  3. `acos_tap {"content_desc": ...}` -> accepted, `state_changed: true`, and the
     **tree** then shows the field `focused: true`
     (`gaten_search_field_focused_after_tap`);
  4. `acos_text {"text": "kayou"}` -> accepted; the reply's `target.node` is that
     field (`content_desc` matches; not masked, because the focused field is plain);
  5. the **next tree** shows the search field's own text `kayou`
     (`gaten_search_field_text_in_tree`) and the results list (`19 results`,
     `gaten_search_results_after_typing`);
  6. `acos_destroy` pass, `SESSION_NOT_FOUND` after.

  Two smoke bugs were found and fixed on the way, both in the smoke, not in the
  engine: (a) the onboarding loop accepted the tab words, which also appear on an
  onboarding page, so the tap landed before Home was ready; it now requires that no
  onboarding control is left; (b) the tap ran before the field's bounds settled; the
  smoke now waits for the bounds to hold still across two trees and polls focus
  (bounded). Evidence: `results/fb1-evidence/focus-probe-live3.txt` (3 Skips, Home
  reached: the content-desc tap focuses the field, `focused: true` on the immediate
  tree, and `kayou` shows in the next tree) and `focus-probe-live4.txt` (2 Skips, the
  app is still on onboarding with a `Skip` control, so no search field is reachable
  yet -- the reason for fix (a)).

### C. The 15 gates and the Gaten 5+5 (final code)

Final sweep on commit `1f53bde`, the installed prefix, `acos verify <scenario>
--engine art`, in five short run-exclusive acquisitions (the first all-at-once sweep
held the lock too long and was stopped; see §6). Evidence:
`results/fb1-evidence/gates-summary.txt` and `sweep-final.sh`.

| scenario | expected | observed |
|---|---|---|
| `v2-art-counter`, `w22-counter-reset`, `w54-dialog-pixels`, `w42-back-frame`, `w15-dialog-menu`, `form-validation`, `suite/T03-render-good`, `v1-offscreen-pager`, `w7-insets-report`, `se1-expect-change-tap` | pass | **pass** (10 of 10) |
| `suite/T03-render-wrong`, `w7-insets-carve-negative`, `negative/se1-expect-change-noop` | ASSERTION_FAILED | **fail, ASSERTION_FAILED** |
| `negative/offscreen-pager-visible` | ELEMENT_OFFSCREEN | **fail, ELEMENT_OFFSCREEN** |
| `p11-progress-timeout` | TIMEOUT | **timeout, TIMEOUT** |
| Gaten `gaten-home`, `gaten-tabs`, `gaten-settings`, `gaten-search`, `gaten-lesson` | pass | **pass** (5 of 5) |
| Gaten `negative/gaten-{home,tabs,settings,search,lesson}-wrong` | ASSERTION_FAILED | **fail, ASSERTION_FAILED** (5 of 5) |

All 25 match their expected code. The sweep script's own `ok` column is wrong for
the failure rows: it compared the verdict *status* (`fail`/`timeout`) with the
expected *code*. The `expected` and code columns are the evidence.

GR4 (4 flows + 4 negatives) was in the earlier sweep on a previous build. It was not
re-run on the final code in this pass (§6).

### D. Full crate test suite

`cargo test --release --offline -p acos-cli -p acos-art -p acos-mcp -p acos-verify
-p acos-protocol --no-fail-fast`.

Run on commit `1f53bde`, every crate in one command per crate (release, offline,
`run-exclusive.sh --task FB1 -- guarded.sh --mem 10G --time 540`). Logs:
`results/fb1-evidence/cargo-test-<crate>.txt`.

| crate | passed | failed | ignored |
|---|---|---|---|
| acos-cli | 187 | 0 | 5 (ART-guest tests, `#[ignore]` since base `5065e82`; not run here) |
| acos-art | 168 | 0 | 0 |
| acos-mcp | 95 | 0 | 0 |
| acos-verify | 85 | 0 | 0 |
| acos-protocol | 69 | 0 | 0 |

The new FB1 unit tests that pass: `focused_search_field_is_echoed_and_named_as_the_target`,
`focused_password_field_is_masked_and_its_text_withheld`,
`unknown_target_is_masked_and_names_no_node`,
`mask_true_forces_masking_on_a_focused_plain_field`; integration
`crates/acos-cli/tests/fb1_live_input.rs` (4 tests, including
`password_typing_is_masked_in_the_reply`).

## 5. Decisions

* **Ambiguity is refused on the live channel only.** A scenario step is a
  reviewed document and keeps "first match in document order, `index` picks
  another" (all 15 gates and the Gaten flows depend on that behaviour); an
  interactive agent tapping blind is the case a silent pick hurts. Both rules
  are documented side by side in the guide.
* **`text` keeps RNTAP's accessible-name rule** (text, else content description,
  among user-visible nodes) and an explicit field match outranks a
  content-description match — the two stages are never mixed, so the candidate
  list of one answer always belongs to one field.
* **`content_desc` is exact**, like a scenario `content_desc: {exact: …}`;
  `contains`/`regex` are deliberately not offered on the live channel (one
  unambiguous spelling is safer than a matcher language).
* **Typing parity, not a new gate.** The refused precondition was a *proxy* for
  "the typing will land somewhere"; the honest proof is the settled result, and
  a field that is not there is now an app-level outcome (nothing changes /
  timeout), not an engine gap.
* **Masking is reply-side, and it follows the focused node.** ACOS does not redact
  the *tree* (the platform reports a password field's text and flags it; rewriting
  that would be a lie about what the app shows). The reply names the focused field
  and echoes only a plain one; a password, `mask: true`, or an unknown target is
  masked with `chars` and a reason. Masking hides only the echo, never the tree.
  Orchestrator ruling applied: no "single password node elsewhere" heuristic.
* **Unknown target is masked, not guessed.** Gaten's tree does report focus after
  the content-desc tap, so a plain search reply is echoed. A screen with no focused
  node gets a masked reply with `node: null`.
* **Typing is proven by the tree.** The smoke's claim is the field's own text and
  the results list in the next tree, never the reply echo.
* **`ACOS_BIN` discovery gained the executable's sibling.** An installed
  `<prefix>/bin/{acos,acos-mcp}` pair is how every real client is registered;
  without this, an MCP server started from an empty outside directory had no
  CLI and answered `INTERNAL` for every session tool (measured in the first
  scratch-install smoke run: `.cache/fb1/mcp/counter.err`).

## 6. Remaining / notes

**Done.** Commits on `fb1-live-input`: `3fc0628`, `9edec1e`, `19e06fc` (previous
agent), `dce3b14` (reviewed draft; its masking heuristic is superseded), `1f53bde`
(final rule and smoke fixes). Not pushed, not merged, `main` not merged in.

**Not covered / open:**

* GR4 (4 flows + 4 negatives) was not re-run on the final code. Its previous run
  predates the masking change. Smallest next action: run the 8 GR4 scenarios with
  the same chunked sweep (`sweep-final.sh`, add a chunk).
* The 5 `#[ignore]` ART-guest tests in acos-cli were not run in this track
  (`cargo test -- --ignored`, under `guarded.sh`).
* `cargo fmt` drift: `serve.rs` already had 5 rustfmt diffs at base `5065e82`; this
  track did not reformat the file (no churn).
* `sweep-final.sh`'s `ok` column is wrong for failure rows (see §4C). Fix: compare
  the code column.
* The first all-at-once sweep held the lock for longer than the ~10-minute limit. It
  was stopped and re-run in chunks, and the stop is recorded. The run-exclusive
  wrapper is fine, but the sweep should be chunked by default.
* Masking policy: applied as the orchestrator ruled (mask unless the focused node is
  a plain field; unknown target masked; no single-password-node heuristic). The cost
  is an echo on every unknown-target reply. Changing it needs a new decision.
* `results/rn/w162a-rn-session.json` and `results/bench/*` churn is not committed.
* Peak RAM during the ART runs: about 28 GiB used of 60 GiB, with 32 GiB available.
  Disk at 83% (`/`), under the 85% stop line.

**Next action.** Orchestrator review of commits `dce3b14`..`1f53bde`, then the GR4
re-run, then the owner decides whether to merge `fb1-live-input` into `main`.
