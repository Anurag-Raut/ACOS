# FB6 handoff — agent feedback: the MCP cheat sheet, canonical tools, route rows, `fidelity: n/a`, logs, hints, JVM dumps

Branch `fb6-agent-docs` in worktree
`/home/Qui/.t3/worktrees/acos/t3-d41b3b08-5ea0b20149a5a9a9`, base `main` `74e4ee9`
(v0.2.0). Track FB6. Source: `~/.claude/acos-feedback/ACOS_AGENT_FEEDBACK.md`
entry 1 items 2, 4, 7, 9, 10, 11, 12; subagent items 9, 12, 13; the Triage
section. Date 2026-11-15.

## 1. What shipped

### (1) `acos_help {"topic": "mcp"}` — generated tool cheat sheet (+ recipes)

`crates/acos-mcp/src/{help.rs,server.rs,descriptors.rs}`, `docs/AGENT_GUIDE.md`,
`docs/MCP.md`.

* New topic **`mcp`** (also the resource `acos://guide/mcp`), **generated from the
  live descriptors on every reply**: a table of every tool `tools/list`
  advertises with required args, optional args, one paste-able example call and
  what the reply contains. `base` is left out of the argument columns (it is the
  identity envelope; the table says `session_id` defaults to this server's own
  session) and each row's example adds the one argument that actually selects
  the target (`apk`, `id`, `text`, `code`, `from`/`to`, …).
* Generated, not written: `help::mcp_topic(&[ToolInfo])` walks
  `available_tool_infos(caps)`. A tool that is added, renamed, gains an argument
  or is hidden changes the table in the same build; a hidden tool is absent by
  construction, so the guide can never teach a call that answers `unsupported`.
* A `tool_returns()` table supplies the "what it returns" clause per tool, and
  `crates/acos-mcp/tests/guide.rs` fails when an advertised tool has no entry —
  so a new tool cannot ship undocumented here.
* `TOPICS` stays the list of markdown sections; `GENERATED_TOPICS = ["mcp"]`,
  `topic_list()` and `is_topic()` are the single source for "which topics exist",
  and `guide_topics`, `guide_unknown_topic`, `resources/list`,
  `resource_uris()` and `read_resource` all use it.
* Recipes at the **top of the `workflow` topic** (and repeated in `mcp`):
  take a screenshot (today: a `capture: {name, format: png, fresh: true}` step in
  `acos_verify`, or CLI `acos observe --screenshot out/x.png` on a live daemon
  session; "Coming: `screenshot: true` on `acos_observe_tree`, returning the PNG
  path and `NO_FRESH_FRAME` instead of a silent no-op"), type into a field, find
  a control by `content_desc`, prove a flow, assert-don't-eyeball.
* The default overview is still the skeleton: **2945 B** of guide text (budget
  3 KiB); the test bound tightened from 4000 to 3072.

### (2) Canonical tools named in the overlapping descriptions

`crates/acos-protocol/src/mcp.rs`. `acos_run` → "NOT THE CANONICAL TOOL — prefer
`acos_launch`"; `acos_observe` → "prefer `acos_observe_tree`"; `acos_action` →
"prefer `acos_tap`/`acos_text`/`acos_key`/`acos_swipe`". All three keep working
unchanged; `docs/MCP.md`'s generated table is regenerated.

### (3) One support row per route; README status

* `docs/AGENT_GUIDE.md` → `limits`: the contradictory "Flutter **and React
  Native** are deferred … treat any result as experimental" bullet is replaced by
  a table with one row per route: **Framework Views** supported/supported,
  **React Native** supported on real ART via the MCP session tools *and* the CLI
  since v0.2.0 (with the note that the tree is the framework `View` hierarchy the
  RN host creates), **Flutter** deferred. Entry-1 item 7 / subagent item 9 are
  the same complaint; both are now answered by that table.
* `README.md`: the dated 2026-11-10 status block ("no Application/Activity
  callback, frame, UI tree, input … exists yet") is replaced by a four-line
  v0.2.0 pointer to `STATUS.md`, `docs/AGENT_GUIDE.md` and `docs/MCP.md`.

### (4) `fidelity: "n/a"` for the meta tools

* `crates/acos-protocol/src/envelope.rs`: additive `Fidelity::NotApplicable`
  (`"n/a"`, serde `rename = "n/a"`), description "not applicable: a meta reply
  about ACOS itself, not about app behaviour", `satisfies(_) = false` (it can
  never satisfy a fidelity requirement), `is_android_verified = false`.
* `acos_help`, `acos_capabilities` and `acos_doctor` now report `n/a` in
  `annotations.fidelity`, `_meta.acos.fidelity` and the tool marker
  (`crates/acos-mcp/src/descriptors.rs`). Schemas regenerated
  (`UPDATE_SCHEMA=1`): `commands.json`, `envelope.json`, `verdict.json`,
  `verify-request.json`. `docs/MCP.md` tool table regenerated and the `n/a`
  rule documented.
* Compatibility: the variant is additive on the wire (an old client that
  matches unknown strings keeps working; `Fidelity` is only ever *produced*
  here, and no parser rejects `n/a` because serde accepts every declared
  variant).

### (5) `acos_logs` defaults: `W` + the app's own package

`crates/acos-cli/src/engine/serve.rs` (the daemon verb every live session tool
goes through), `crates/acos-cli/src/session.rs` (the CLI's own path),
`crates/acos-mcp/src/descriptors.rs` (schema/description).

* Default view: records at priority `W` and above **plus** anything whose tag or
  message names the launched package (a pure helper `keep_log_record`, unit
  tested). The reply carries `filter: {min_priority, package, default_view,
  dropped, note}` and `total` (buffer) vs `count` (returned).
* Nothing is lost: the guest buffer is untouched, `logs/logcat.txt` in a verify
  run is still the full log, `{"all": true}` or `min_priority: "V"` returns all
  of it, `tag` narrows further (and does not lift the priority floor — that is
  what `all`/`V` are for).
* Documented in the guide (workflow step 6, limits), `docs/MCP.md`, and the tool
  schema.

### (6) Dedicated hint for an implicit-intent `startActivity`

`crates/acos-art/src/launch.rs` (the guest marker's own `detail` now reaches the
reply message), `crates/acos-cli/src/failure.rs` (`art_error`).

When `UNSUPPORTED_ANDROID_API`'s message says the request had **no explicit
component**, the hint is no longer the generic "avoid the API / mark unsupported"
but: *the app started an activity through an implicit intent and no other apps
exist in this guest — there is no Settings, browser, share sheet, file picker or
dialer to resolve it against; such screens are untestable here, which is a
capability gap, not an app bug; do not stub the Activity: assert up to the intent
(`assert_log` on the app's own line, or `assert_state` on the control that
triggered it) and report the screen beyond it as unsupported.* A real API gap
still gets the generic hint (both directions unit tested,
`implicit_intent_start_activity_has_a_dedicated_hint`).

### (7) `replay_pid*.log` in the repo root — root cause and fix

New `crates/acos-art/src/jvm.rs`; `app.rs`, `session.rs`, `launch.rs`; `.gitignore`.

**Root cause (evidence below):** they are HotSpot *compiler replay* dumps
written by the **`javac` JVM that ACOS spawns to build its guest sources**, into
the process's inherited working directory — i.e. whatever directory ACOS was run
from. `javac` is started by `acos-art` for the three guest builds
(`session-guest` in `session.rs`, render-app guest in `app.rs`, launch guest in
`launch.rs`) and for `d8`'s own JVM (the Android build-tools `d8` script is a
`java` wrapper that forwards leading `-J` flags). Evidence:

* **Contents** name `com/sun/tools/javac/...` throughout (`instanceKlass
  com/sun/tools/javac/jvm/ClassReader`, `ciMethodData java/util/HashMap get`),
  with `staticfield`/`ciInstanceKlass` records (1375/131 in one file), the
  HotSpot header `JvmtiExport can_access_local_variables 0` /
  `# N ciObject found`, and the last record is an in-flight
  `compile com/sun/tools/javac/comp/Check checkMethod (...)` — i.e. a dump taken
  while the C2 compiler was compiling javac's own `Check.checkMethod`. 180–350 kB
  per file, **no `hs_err_pid*.log` alongside** them.
* **Where the name comes from**: the installed JDK 17 product VM contains the
  strings `ReplayDataFile`, `ReplayCompiles`, `replay_pid%p.log` and
  `replay_pid%p_compid%d.log`; `ReplayDataFile` defaults to empty and
  `DumpReplayDataOnError` to `true`. A replay file is written **only when the JVM
  is told where to put it** (`-XX:ReplayDataFile=…`, with `%p` expanded to the
  pid — which is exactly the shape of the existing names, no `_compid` suffix)
  **and** the VM dies with a fatal error while a compiler thread is mid-flight.
  Verified on this host: a `-XX:ReplayDataFile=…` JVM that is aborted *does not*
  produce a replay file, while its `hs_err_pid<pid>.log` is written normally
  (SIGSEGV and `-XX:+CrashOnOutOfMemoryError` both tested). So the flag was
  present in the **environment of those runs** (a `JAVA_TOOL_OPTIONS`-style
  experiment option inherited by every child JVM); it is not in any checked-in
  script, not in `tools/`, and not in today's environment.
* **When**: 21 files, mtimes 2026-11-12 07:06:34, 07:08:39, 07:10:0x, 07:12
  (four batches — 4, 6, 9, 2 files, pids 100–600 apart, i.e. parallel JVMs in one
  second). That window matches exactly the cold-launch javac compilations of the
  W31/W32/W34 acceptance runs from `.cache/w31-accept.sh` (07:04),
  `.cache/w34/*` baselines (07:10) and `.cache/w33-full.log` (07:14): each
  `acos verify` launch compiled the guest sources, each compile inherited the
  checkout root as cwd, and a compile that died left its dump there. `hs_err`
  files from the same events were evidently deleted at the time (no `hs_err*`
  remains); the `replay_pid*` names were not recognised as crash noise.

**Fix (durable, crash-cause independent):** every JVM ACOS spawns is told where
to dump. `crate::jvm::dump_args()` creates `<root>/.cache/art/jvm-dumps/`
(`<root>` = `$ACOS_HOME` in an installed layout, else the checkout — the same
root as the build cache) and returns
`-J-XX:ErrorFile=<dir>/hs_err_pid%p.log` and
`-J-XX:ReplayDataFile=<dir>/replay_pid%p.log`, appended **first** to every
`javac` and `d8` invocation (the `-J` form both tools forward to the JVM; `%p`
keeps parallel compiles apart). Result: no dump can ever land in the caller's
working directory again, the dumps are still captured when a build tool really
does crash, and they are attributable by path. `.gitignore` now covers
`replay_pid*.log`, `hs_err_pid*.log` and `core.[0-9]*`.
Verified: the exact `-J` flag pair is accepted by this `javac` (rc=0), a JVM
fatal error writes `hs_err_pid<pid>.log` into the routed directory and nothing
into the cwd, `.cache/art/jvm-dumps` is created by the `jvm::` unit test, and no
`replay_pid*.log`/`hs_err_pid*.log` appears at the worktree root after the gate
runs.

**Not changed (deliberately):** the crash itself. A JVM that dies during a C2
compile of javac's `Check.checkMethod` under a memory-capped guard is a JDK-side
failure we cannot reproduce `-XX:ReplayDataFile`-free on this host; routing
keeps it out of the repository and keeps the evidence, and pinning `-J-Xmx` for
`javac` was *not* done (it would silently change build behaviour for large
classpaths). Recorded here so it can be revisited with a replay harness if it
recurs.

### Also (environment, not product)

* `crates/acos-cli/tests/w50_serve_art.rs`: the test's own unix socket path
  overflowed `SUN_LEN` (108 chars) from this worktree
  (`…/t3-d41b3b08-5ea0b20149a5a9a9/.cache/w50-test-unknown-session-<pid>/serve.sock`
  = 108 chars, main checkout = 74), so `acos serve` could not bind and the test
  failed on path length, not behaviour. `scratch()` now falls back to a short
  hashed directory name when the readable one does not fit and asserts the
  resolved socket path ≤ 107 chars. No assertion was weakened.
* The worktree's `fixtures/build` was a **symlink** into the main checkout, which
  the path guard correctly rejects (`UNSAFE_PATH`: symlink escapes the allowed
  roots) — `acos-mcp`'s e2e test failed for that reason alone. It is now a real
  copy inside the worktree (gitignored build output).

## 2. Verification

All runs through `tools/art-runtime/run-exclusive.sh --task FB6 -- tools/guarded.sh …`
(builds via `guarded.sh --mem 14G --vmem 40000000`), release profile, offline,
`CARGO_TARGET_DIR=<worktree>/.cache/g3f1-target`.

| check | result |
|---|---|
| `cargo test --release -p acos-mcp` | **99 passed, 0 failed** (13 suites; `guide.rs` grew from 7 to 11 tests) |
| `cargo test --release -p acos-cli` | **175 passed, 0 failed** (16 suites) |
| `cargo test --release -p acos-protocol` | **66 passed, 0 failed** (7 suites, incl. `schemas_are_up_to_date` after `UPDATE_SCHEMA=1`) |
| `cargo test -p acos-art --lib jvm::` | 1 passed (dump dir created, absolute, pid-scoped, not `/tmp`) |
| gate `v2-art-counter` (`--engine art`) | **pass**: 11/11 steps, `fidelity: art-reference`, `wall_ms 3553` |
| gate `gaten-home` (React Native, `--engine art`) | **pass**: 14/14 steps, `fidelity: art-reference`, `wall_ms 6131` |
| live logs default (counter APK, daemon session) | `total 82, count 59, dropped 23`, `filter.min_priority "W"`, `filter.package "dev.acos.fixtures.counter"` |
| live logs `--min-priority V` | `total 82, count 82, dropped 0, default_view false` |
| live logs `--tag AcosCounter --min-priority V` | `count 3` |
| implicit-intent hint | unit test asserts the dedicated text for the no-explicit-component message and the generic text for a real API gap |
| JVM dump routing | routed `-J` flags accepted by javac; forced fatal error wrote `hs_err_pid<pid>.log` into the dump dir, cwd clean; no dump files at the worktree root |

### `acos_help` sizes (measured on the built server)

| topic | `result.guide` | whole tool reply |
|---|---|---|
| default (`{}`) → skeleton | **2945 B** (budget 3 KiB) | 9911 B |
| `mcp` (generated) | 6413 B | 13394 B |
| `workflow` | 5684 B | 12710 B |
| `limits` | 4817 B | 11759 B |
| `all` | 26082 B | 33521 B |

The 3 KiB budget is the guide text the agent reads (the skeleton); the rest of a
default reply is the existing envelope (`error_playbook`, `fidelity_rules`,
`supported`/`unsupported`, `backend`, `compatibility`). Those duplicate the
guide's own text; folding them away on the default path would cut the default
reply from 9.9 kB to ~4 kB and is the obvious next trim if the budget is meant to
cover the whole reply — not done here because the acceptance was the overview
size (the guide text) and the fields are asserted by existing tests.

## 3. Decisions

* `mcp` as a **generated** topic rather than a markdown section: the table must
  track `tools/list` per build, and `TOPICS` (markdown sections) vs
  `GENERATED_TOPICS` keeps the existing "every topic slices a real section" test
  honest while `topic_list()` remains the one list every access path uses.
* `Fidelity::NotApplicable` (`"n/a"`) as an additive enum variant rather than
  documenting `static-analysis`: the schema change is mechanical
  (`UPDATE_SCHEMA=1`) and nothing on the wire rejects it.
* Logs: default-filter the **view**, never the buffer, and report the applied
  filter in the reply so an agent can tell "the app said nothing" from "the
  default hid it".
* JVM dumps: route both `ErrorFile` and `ReplayDataFile` into ACOS scratch for
  every JVM ACOS spawns (product code), rather than hunting the one-off
  environment option that produced the 21 files; the crash cause is documented
  above and not papered over.

## 4. Remaining / follow-ups (not in FB6)

* The default `acos_help` reply is 9.9 kB because of duplicated envelope fields
  (see §2): fold `workflow`/`markdown`/`error_playbook`/`fidelity_rules` away on
  the no-topic path if the whole-reply budget is meant to be 3 kB.
* `acos_text` on a live session is still a gap **on the legacy route**; the guide
  says so and points at the scenario `type:` step. FB1 owns the live channel.
* Screenshot through MCP (`screenshot: true` on `acos_observe_tree`, plus
  `NO_FRESH_FRAME` instead of a silent no-op) is described in the recipes as
  "Coming" — FB2 owns it.
* The `javac`-JVM crash that produced the original dumps cannot be reproduced on
  demand here (product VM, no replay flag); if it recurs, the dumps now land in
  `.cache/art/jvm-dumps` together with `hs_err` and are usable as a repro.
* `replay_pid*.log` files in `/home/Qui/Projects/acos` (21 of them, 2026-11-12)
  were **not** deleted — they are the orchestrator's to clean up.

## 5. Host usage

* Peak host memory observed: **28 GiB used / 60 GiB** (`MemAvailable` 32–33 GiB,
  47 % used) during the gate runs; every run under
  `guarded.sh --mem 10G` (builds `--mem 14G --vmem 40000000`), `--time 900`.
* Disk: `/` went from 83 % to **85 %** used (749 GB of 945 GB, 149 GB free) —
  the 644 MB `fixtures/build` copy inside the worktree plus the release build
  target. No further growth observed; `.cache` scratch used by this track is
  `.cache/g3f1-target` (12 GB, pre-existing) and `.cache/art/jvm-dumps` (empty).
  Nothing under `/tmp`.
