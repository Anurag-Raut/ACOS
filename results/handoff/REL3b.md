# REL3b — the rest of the agent-friction set (F5, F6, F10, F11, F12)

Branch `rel3b-agent-friction` (from `main` = c517a76). Worktree
`/home/Qui/.paseo/worktrees/3oxa6xao/s3ab`, target dir `.cache/s3ab-target`.
REL3a owns the engine problems (session tools → ART by default, `acos_verify`
`engine` arg/schema, unique session ids, ABI gate); nothing in
`crates/acos-mcp/src/{session_backend,daemon,rn_backend}.rs` and no engine
paragraph of `docs/AGENT_GUIDE.md` was touched here.

Evidence files: `results/rel3b/` (`gaten-inspect-launcher.json` = F6,
`shorthand.json` / `undeclared.json` = F5, the three gate verdicts, and
`help-tools.jsonl` = F12 `acos_help` + `tools/list` replies).

## F5 — a documented activity shorthand was an app FAIL

`launch: {activity: com.gaten.app/.MainActivity}` returned
`ASSERTION_FAILED "activity `com.gaten.app/.MainActivity` is not declared"`
while `details.declared_activities` contained `com.gaten.app.MainActivity` and
`details.resolved` echoed the shorthand unexpanded.

Cause: `acos_verify::backend::resolve_component` required the package part of a
`pkg/.Cls` component to equal the **manifest** package. Gaten's manifest package
is `com.gaten` while its components live in `com.gaten.app`, so a valid Android
component name resolved to `None`.

Fix (`crates/acos-verify/src/backend.rs`, `src/executor.rs`):

* expansion follows Android's own rules and cites them: `pkg/.Cls` → `pkg.Cls`
  (`.cache/aosp/base/core/java/android/content/ComponentName.java`,
  `unflattenFromString`: a leading `.` is expanded against the package part of
  the component string, not against the manifest package); `pkg/Cls` with a
  dotless class, `.Cls` and a bare `Cls` → relative to the package
  (`PackageParser.buildClassName`); a fully-qualified class stays as written.
  A component prefix that differs from the manifest package is accepted.
* the launch step now hands the **expanded** component to the backend, so no
  engine has to re-implement `ComponentName` expansion.
* an undeclared launch target is a scenario/setup error, not an app assertion:
  `INVALID_SCENARIO`, status `error`, `verdict: error`, hint pointing at the
  scenario, `details = {requested, resolved, declared_activities,
  launcher_activity, error_class: "invalid_scenario"}`. The
  "no activity and no launcher" case is `INVALID_SCENARIO` too.

Tests: `acos-verify` unit tests
`resolve_component_expands_android_component_shorthand` (4 accepted spellings,
3 degenerate rejected) and `launch_gate_tests` (documented shorthand passes the
gate; undeclared → `INVALID_SCENARIO`, asserts it is *not* `ASSERTION_FAILED`;
no-launcher case). Live, on `--engine art`:

| scenario | activity | result |
|---|---|---|
| `.cache/rel3b-scenarios/gaten-shorthand.yaml` (copy of `scenarios/gaten-home.yaml`) | `com.gaten.app/.MainActivity` | **pass**, 14/14 steps (`results/rel3b/shorthand.json`) |
| `.cache/rel3b-scenarios/gaten-undeclared.yaml` | `com.gaten.app/.NopeActivity` | `status=error`, `code=INVALID_SCENARIO`, message "`com.gaten.app/.NopeActivity` resolves to `com.gaten.app.NopeActivity`, which … does not declare", `details.resolved=com.gaten.app.NopeActivity` (`results/rel3b/undeclared.json`) |

`docs/ERROR_CODES.md` documents the `INVALID_SCENARIO` launch gate; the guide's
scenario section lists the accepted spellings.

## F6 — `launcher_activity: null`, every activity `intent_filters: []`

Gaten declares **no** MAIN/LAUNCHER filter on any `<activity>`. Its launcher
entries are three `<activity-alias>` elements (`LauncherIconSystem`,
`LauncherIconLight`, `LauncherIconDark`) that target
`com.gaten.app.LauncherActivity`; only the light one is `enabled`. `acos inspect`
read `manifest.activities` only, so it reported no launcher and no filters
(`aapt2 dump badging` reports no `launchable-activity` for the same reason —
`crates/acos-services/src/manifest_resolve.rs` already knew this for `acos-art
launch`, the `acos-apk` model did not).

Fix (`crates/acos-apk/src/manifest.rs`):

* `<activity-alias>` is parsed into `Manifest::activities` with
  `ComponentKind::ActivityAlias` (AOSP's `PackageParser.parseActivityAlias` adds
  it to `owner.activities`), carrying `target_activity`
  (`ActivityInfo.targetActivity`) and `enabled`;
* every component now reports `target_activity` and `enabled` (inspect JSON and
  the protocol `ComponentSummary`, plus `ComponentKindSummary::ActivityAlias`);
  aliases' `<intent-filter>`s are reported, enabled or not;
* `Manifest::launcher_activity()` resolves deterministically and matches
  `manifest_resolve`/`LauncherApps`: enabled aliases first, then enabled
  activities, declaration order within a group, and
  `<application android:enabled="false">` disables everything.

**Acceptance wording vs. the manifest.** The task says inspect should show
`launcher_activity: com.gaten.app.MainActivity`. It must not: `MainActivity` has
no MAIN/LAUNCHER filter in this APK (verified with
`aapt2 dump xmltree`/`badging`: no `launchable-activity`; the only launcher
filters sit on the three aliases targeting `LauncherActivity`). The honest
answer, now reported, is `com.gaten.app.LauncherIconLight` with
`target_activity: com.gaten.app.LauncherActivity` — which is what a launcher
Intent names and what `acos-art launch` already used. Reporting `MainActivity`
would be a fabricated launcher entry; `scenarios/gaten-home.yaml` can still
launch `MainActivity` explicitly (it passes).

Evidence: `results/rel3b/gaten-inspect-launcher.json`
(`launcher_activity: "com.gaten.app.LauncherIconLight"`, three
`activity_alias` entries with their MAIN/LAUNCHER filters and targets).
Tests: `manifest::launcher_tests` (enabled alias wins, disabled aliases still
carry filters, plain launcher still resolves, `<application enabled=false>`),
`tests/fixture_apk.rs::gaten_apk_reports_its_alias_launcher` (real APK, skips if
absent), plus the pre-existing DOM tests.

## F10 — `flags` were useless and unrecorded

`acos_verify {"flags": ["--native"]}` produced `INTERNAL "a value is required
for '--flag <FLAGS>'"` (the MCP passed `--flag --native`), and `["engine=art"]`
"changed nothing". Decision: **keep the parameter, define it, validate it and
record it** — `VerifyRequest.flags` is part of the protocol contract and already
lands in `verdict.reproducer.flags`, so it is not dead; it was undocumented and
unvalidated.

* `acos_protocol::commands::validate_reproducer_flag`: `NAME=VALUE`, name
  `[A-Za-z][A-Za-z0-9_.-]*`, non-empty value without whitespace/quotes, ≤ 256
  bytes. One implementation shared by CLI and MCP, so they cannot disagree.
* MCP `acos_verify`: a malformed entry is a structured `INVALID_ARGUMENT` with
  `details = {field: "flags", index, value, reason}` and **nothing is
  dispatched**; `["engine=art", "variant.pre-fix=1"]` reaches the CLI as
  `--flag engine=art --flag variant.pre-fix=1`.
* `acos verify --flag` validates the same way (`INVALID_ARGUMENT` envelope,
  `value_name = NAME=VALUE`, doc comment says flags are metadata, not options).
* `repro.sh` now replays the recorded flags (`  --flag engine=art`), and
  `repro.json` already carried them.
* `docs/MCP.md`/the schema description say NAME=VALUE.

Tests: `flag_tests` (5 accepted, 10 rejected cases), `repro_sh_replays_the_recorded_flags`
(repro.sh lines, order, continuation, empty-flag case),
`tests/bounded.rs::flags_are_name_value_metadata_and_malformed_ones_are_invalid_argument`
(fake `acos` records argv: a valid flag arrives as `--flag engine=art`; four
malformed values are `INVALID_ARGUMENT` with the right `details` and never start
the backend).

`acos_run`'s schema has no `flags` property (only `VerifyRequest` does), so
there was nothing to fix there; additional properties are ignored by the tool,
which is why the agent saw "no change".

## F11 — one failing verify = ~14k tokens

`bound` (`crates/acos-mcp/src/bound.rs`) runs once, in `tool_result`, for every
tool reply:

* every string longer than `ACOS_MCP_MAX_TEXT` (default **4000 bytes**; `0`
  disables) is head/tail-capped on line boundaries (head ~1/3, tail ~2/3, so the
  first frames and the deepest `caused by:`/last native lines survive);
* the marker names the JSON path, the original size, what was kept and where the
  full text still lives (`result.verdict_path`/`artifacts_dir` when the envelope
  carries one, else "the run's artifacts directory");
* an **identical** repeat is replaced by
  `[ACOS MCP: identical N-byte text already shown at /error/message; not repeated]`,
  so the full text appears once;
* the reply carries `_meta.acos_mcp.reply_bounded =
  {truncated_strings, duplicate_strings, saved_bytes, cap_bytes}` so a silent
  truncation can never be read as the real text;
* `acos_help`'s `guide`/`markdown` are exempt (a documentation fetch is
  deliberate, and `topic: "all"` must return everything).

The CLI's `--json` output and the artifacts on disk are untouched: the bound is
an MCP-reply concern only.

Tests: `bound::tests` (7: short untouched, cap+marker+artifact path, one full
copy + two back-references, configurable/0, help exempt and an error tail not,
repeated short strings not de-duplicated) and
`tests/bounded.rs` (end-to-end through the server with a fake `acos` emitting a
220-frame java + 420-line native tail in three places: reply < 24 KB instead of
~150 KB, `truncated_strings=1`, `duplicate_strings=2`, saved > 40 KB, on-disk
envelope still > 20 KB; and a `max_text_bytes = 0` reply that is unbounded).

## F12 — help envelope, versions, stub tools

**Help.** `docs/AGENT_GUIDE.md` gained a `<!-- topic:overview -->` block (the
three status rules, the loop in ten lines, the topic list with one-line
descriptions, "call `acos_help {\"topic\": \"workflow\"}` next"). `acos_help`
with **no topic** returns that skeleton; `{"topic": "<name>"}` one section;
`{"topic": "all"}` the whole guide; an unknown topic returns the skeleton with
`guide_unknown_topic: true`. The guide is still single-sourced (`include_str!` +
topic markers); `acos://guide` (a deliberate resource read) still serves the
complete guide.

| reply | before | after |
|---|---|---|
| `acos_help {}` — guide text | 20,305 bytes (REL1b measured 17,476 chars for the older guide) | **2,746 bytes** (13% of the guide) |
| `acos_help {}` — whole envelope | — | 9,620 bytes |
| `acos_help {"topic": "all"}` — guide text | 20,305 | 20,305 (full, opt-in) |

Tests: `tests/guide.rs` rewritten to the new contract (skeleton < 4000 bytes,
contains the rules/loop/topic names, every topic still slices a real section,
`all` is the full guide, unknown → skeleton, skeleton lines are guide lines,
`acos://guide` = full guide, `acos://guide/overview` listed).

**Version.** Workspace `version = "0.2.0"` (was 0.1.0), so `acos --version`,
`doctor.acos_version`, `SERVER_VERSION` and the reproducer's `acos_version` all
report 0.2.0 next to `install_version 0.2.0`. `acos --version` →
`acos 0.2.0`.

**Stub tools.** `tools/list` now advertises only tools this build can run:
`acos_capture`, `acos_assert`, `acos_events`, `acos_profile` and
`acos_capability_report` are hidden (19 tools advertised instead of 24);
`acos_run`/`acos_observe`/`acos_action`/`acos_reset`/`acos_destroy` stay, because
the in-process session backend implements them. Names remain reserved: the
protocol descriptors are unchanged, `docs/MCP.md` still lists every command, and
calling a hidden tool returns the same structured `unsupported`
(`error.details.available = false`).

Tests: `protocol.rs::tools_list_advertises_only_what_this_build_can_run`
(every advertised tool is `available`, the stubs are absent, all protocol names
still reserved, `acos_assert` still answers `UNSUPPORTED_BACKEND`),
`e2e.rs` updated the same way (session-backed commands stay listed).

## Test evidence

`cargo test --release --offline -p acos-apk -p acos-verify -p acos-mcp -p acos-cli -p acos-protocol`
through `run-exclusive.sh` + `guarded.sh`: **456 passed, 0 failed, 0 panicked**
(45 suites; log `.cache/rel3b-tests.log`). Schemas regenerated
(`commands.json`, `verify-request.json`).

Gates, all on `--engine art` (`results/rel3b/`):

| gate | result |
|---|---|
| `scenarios/v2-art-counter.yaml` | pass, 11 steps, fidelity `art-reference` |
| `scenarios/w22-counter-reset.yaml` | pass, 9 steps, fidelity `art-reference` |
| `scenarios/gaten-home.yaml` | pass, 14 steps, fidelity `art-reference` |

## Decisions / notes for the reviewer

1. **F6 acceptance wording.** `launcher_activity` for Gaten is
   `com.gaten.app.LauncherIconLight` (the enabled launcher alias), not
   `com.gaten.app.MainActivity`; `MainActivity` declares no launcher filter and
   inventing one would be a lie. See the section above; `aapt2 dump
   badging`/`xmltree` and the alias targets are the evidence.
2. **`INVALID_SCENARIO`'s `class` is still `harness`.** The code/status are right
   (`error`, retryable false); its failure class comes from the protocol's
   existing table, and changing a class is a taxonomy change outside this track.
3. **`fixtures/build` was a symlink** into the main checkout in this worktree,
   which makes `ACOS_ALLOWED_ROOTS`-based tests fail with `UNSAFE_PATH`
   (pre-existing, REL1b hit it too). It is now a hardlinked real directory
   (untracked; nothing to commit). Because its `acos-art` binary was stale, the
   ART guest build failed on `SoftwareKeyStore` until `cargo build -p acos-art`
   ran on this branch.
4. `bound` uses a byte cap, not a line count: `ACOS_MCP_MAX_TEXT` is the knob and
   the marker states the exact bytes kept, so the truncation is auditable.
5. The `_meta.acos_mcp.reply_bounded` key is additive; MCP clients ignore unknown
   `_meta`.

## What is left (not done here)

* REL3a's engine items (session tools → ART by default, `engine` arg/schema,
  unique session ids, ABI gate, engine paragraphs of the guide).
* `ErrorCode::InvalidScenario`'s failure class (`harness`) — a taxonomy
  decision, noted above.
* Nothing is pushed or merged; no MCP registration or `~/.local` change was made.
