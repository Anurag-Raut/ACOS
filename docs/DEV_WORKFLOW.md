# ACOS development workflow (for AI agents)

**Dated 2026-11-10.** This is the exact develop → build → run → verify loop an
AI agent follows with ACOS **today**, and what each step maps to. It is written
against the working tree as of this date; every step is labelled
**working**, **partial** or **not working yet** so the doc does not overstate the
runtime. It complements `docs/AGENT_API.md` (the JSON/MCP contract) and
`docs/MCP.md` (the MCP server).

The one-line version of ACOS today: **static analysis works and is verified;
execution of an Activity does not.** An agent can write a real Android app,
build a real signed APK, inspect it, and run a scenario against the ART oracle
oracle replay — but the native backend stops at the first UI step.

---

## 0. The loop

```
        ┌──────────────────────────────────────────────────────────────┐
        │ 1. write app source   fixtures/apps/<name>/                  │
        │ 2. build APK          tools/build-apps.sh                    │
        │ 3. static inspect     acos inspect  (MCP acos_inspect)       │
        │ 4. capture ART refs   tools/oracle/capture-app-reference.sh  │
        │ 5. write scenario     scenarios/<name>.yaml                  │
        │ 6. verify             acos verify  (MCP acos_verify)         │
        │ 7. read evidence      verdict.json / steps / frames / tree   │
        │ 8. fix and repeat                                             │
        └──────────────────────────────────────────────────────────────┘
```

The honest status of each step is in §9. In short: 1–3 work, 4 works, 5 works,
6 is **partial** (native backend has no UI/input), 7 works for whatever step
the engine reached.

---

## 1. Write the app source

Location: `fixtures/apps/<name>/`

```
fixtures/apps/<name>/
  AndroidManifest.xml                 package, launcher activity, minSdk 26, targetSdk 34
  res/values/strings.xml
  res/layout/*.xml
  src/<package path>/*.java           plain framework APIs only (no AndroidX)
```

Rules that keep the fixtures buildable without Gradle:

* **No AndroidX / no support library.** Only `android.*` classes from
  `platforms/android-34/android.jar`. Everything on the framework boot
  classpath is fair game (`Activity`, `View`, `ListView`, `SharedPreferences`,
  `Intent`, `AlertDialog`, `Toast`, `Log`, …).
* Use `javac --release 11` (the build script does).
* Give every observable widget a stable `@+id/...`; the reference capture and
  the scenarios select by id (or by exact text for `ListView` rows).
* Log lifecycle and actions with a unique tag (e.g. `AcosCounter`,
  `AcosTodo`, `AcosMulti`, `AcosForm`) so `assert_log` can order them.

Existing examples: `fixtures/apps/hello-views`, `counter`, `todo-list`,
`multi-activity`, `form-validation`, `hello-jni`.

**Status: working.** It is ordinary Android app source.

---

## 2. Build the APK

```bash
tools/build-apps.sh
```

It compiles resources with `aapt2`, Java with `javac`, DEX with `d8`, then
`zipalign` + `apksigner` (throwaway debug keystore under
`fixtures/build/apps/debug.keystore`, never committed). Outputs are gitignored:

```
fixtures/build/apps/<name>.apk
```

The script runs `apksigner verify` and `aapt2 dump badging` for every APK, and
re-execs itself under `tools/guarded.sh --mem 6G --time 1800` (ADR-0018).

A device/emulator install smoke test runs **only** if `adb devices` already
lists one; the script never starts an emulator.

Manual checks (what acceptance uses):

```bash
SDK=/home/Qui/Android/Sdk
$SDK/build-tools/35.0.0/apksigner verify --verbose fixtures/build/apps/<name>.apk
$SDK/build-tools/35.0.0/aapt2 dump badging fixtures/build/apps/<name>.apk
```

**Status: working.**

---

## 3. Static inspect

```bash
./target/release/acos inspect fixtures/build/apps/<name>.apk --json
# MCP: acos_inspect { "apk": "fixtures/build/apps/<name>.apk" }
```

Returns the typed `InspectResult` (package, versionCode/Name, minSdk/targetSdk,
label, launcher activity, components, permissions, DEX entries, resources) in a
`status: pass` envelope with `fidelity: static-analysis`. It never executes
code, so a `pass` here means "the APK parsed", not "the app works".

**Status: working.** This is the one command an agent can always rely on.

---

## 4. Capture ART reference evidence

Authoritative `art-reference` evidence is captured from the real app running on
real ART (API 34 x86_64 emulator). It is the golden truth the ACOS runtime must
eventually reproduce.

```bash
tools/oracle/emulator.sh start                 # headless, <=2048 MB, one instance
tools/oracle/capture-app-reference.sh          # captures every fixture app
tools/oracle/emulator.sh stop                  # always shut it down afterwards
```

Per app it writes `fixtures/reference/<app>/art-api34-x86_64/`:

```
manifest.json                artifact list (size + sha256), device config, commands
device-config.json           resolution / density / locale / font scale / build
am_start_initial.txt         `am start -W` cold-start timings
screenshot_*.png             `adb exec-out screencap -p`
ui_*.xml / ui_*.json         uiautomator hierarchy (raw + parsed nodes)
observed-texts-*.txt         class | resource-id | text | bounds per state
logcat_*.txt                 the app's log tag lines
lifecycle-summary.txt        ordered lifecycle/action log lines
meminfo_initial.txt          dumpsys meminfo PSS breakdown
display.txt                  dumpsys window displays
```

Fidelity label: **`art-reference (emulator, API 34, google_apis x86_64)`** —
never a JVM (`jvm-reference`), never a mock. See `fixtures/reference/README.md`.

**Status: working.** This is the oracle, not the product.

---

## 5. Write the verification scenario

Location: `scenarios/<name>.yaml` (positive) and
`scenarios/negative/<name>-*.yaml` (deliberately failing). The format is
`docs/AGENT_API.md` §9; the JSON Schema is
`crates/acos-protocol/schema/scenario.json`.

Rules:

* `version: 1`; `expectation: pass` for positives and `fail`/`non_pass` for
  negatives.
* `display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}` —
  the ART capture configuration, so bounds line up.
* Use the exact texts and bounds from `observed-texts-*.txt` / `ui_*.json`.
* `assert_log` with `tag` + ordered `patterns` proves lifecycle order;
  `since_previous_step: false` scans the whole session, `true` scopes to the
  previous step.
* Negative scenarios live under `scenarios/negative/` and **must** fail; a
  passing negative is a harness bug, never a successful test.

Validate without running anything:

```bash
tools/guarded.sh --mem 4G --time 900 -- cargo test -p acos-protocol
```

`every_checked_in_scenario_parses_and_validates` loads every YAML, runs the
validator, and asserts positives expect `pass` and negatives expect `fail`.

**Status: working** (parsing + validation). The negative variants are also
proven to fail against the reference replay backend, e.g.:

```bash
./target/release/acos verify scenarios/negative/counter-wrong-count.yaml \
  --reference fixtures/reference/counter/art-api34-x86_64 --json
# -> verdict: fail, error: ASSERTION_FAILED, expected "99" got "0"
```

---

## 6. Verify

```bash
# native backend (default): real ACOS runtime
./target/release/acos verify scenarios/<name>.yaml --json --artifacts out/<name>

# reference replay backend: replay the checked-in ART capture
./target/release/acos verify scenarios/<name>.yaml \
  --reference fixtures/reference/<name>/art-api34-x86_64 --json
# MCP: acos_verify { "apk": ..., "scenario": ..., "reference": ... }
```

The runner asks a `Backend` for one operation per step and checks the matching
capability first (`docs/AGENT_API.md` §17). A missing capability yields
`status: unsupported`, code `UNSUPPORTED_BACKEND`, and execution **stops** —
later assertions are never evaluated against stale state.

### Backend status today

| backend | fidelity | what it actually does | status |
|---|---|---|---|
| `acos-native` (default) | `native-runtime` | static manifest + `launch` to the M2 `attachApplication` point (`acos-runtime`) | **partial** |
| `acvm-reference` (`--reference`) | `art-reference` | replays the checked-in ART UI tree + log stream; **no** input/reset/environment | **partial** |

Native example (this is what an agent sees today for any UI scenario):

```
step 0 launch     pass
step 1 wait_for   unsupported  UNSUPPORTED_BACKEND (missing capability `ui.tree`)
verdict: unsupported
```

That is a **truthful partial**, not a failure of the app. The reference replay
is not an execution backend either; it exists so the assertion logic and
false-pass prevention can be exercised before M2/M3 land. Its state names
(`ui_initial.json`, `ui_after_click1.json`, …) are currently specialised to
`hello-views`; for the other fixtures it serves the initial state only.

**Status: partial.** `verify` runs and reports honestly; the native UI/input/
capture/log capabilities are **not working yet**.

---

## 7. Read the evidence

`--artifacts <dir>` (default `out/<scenario-stem>`) receives:

```
verdict.json          the full typed Verdict
steps/NN-<kind>.json  one record per executed step
frames/<seq>.png      every captured frame
tree/<seq>.json       the UI tree bound to that frame
logs/logcat.txt
events.jsonl          append-only event stream
resources.json        ResourceReport
repro.sh / repro.json APK+scenario sha256, version, flags, isolation, display
```

Rules an agent must follow (`docs/AGENT_API.md` §10.1, §17.3):

* A `pass` with missing frame/tree/step evidence is **invalid**
  (`ARTIFACTS_INVALID`). Never hand-edit `verdict.json`.
* A screenshot and the tree it is compared against share one `frame_seq`; a
  stale frame is `NO_FRESH_FRAME`, never an old image.
* A tap is only accepted when the same completed frame shows the change in both
  the UI tree and the screenshot.

**Status: working** for whatever steps the engine reached.

---

## 8. Fix and repeat

Follow the per-error playbook in `docs/AGENT_API.md` §13. The important
distinction:

* `unsupported` / `UNSUPPORTED_*` → the **engine** cannot do it yet. Report the
  gap; never rewrite the app or the scenario to hide it.
* `fail` / `ASSERTION_FAILED` / `ELEMENT_NOT_FOUND` → the **app or the
  expectation** is wrong. Inspect the linked frame/tree and the assertion diff.
* `crash` / `APP_CRASHED` / `JAVA_EXCEPTION` → not an assertion failure.

---

## 9. Status of every step (2026-11-10)

| step | tool | MCP tool | status |
|---|---|---|---|
| write app source | editor | — | working |
| build APK | `tools/build-apps.sh` | — | working |
| verify signature/badging | `apksigner`, `aapt2` | — | working |
| static inspect | `acos inspect` | `acos_inspect` | working |
| DEX console run | `acos run-dex` | `acos_run_dex` | working |
| ART reference capture | `tools/oracle/*` | — | working |
| scenario validation | `cargo test -p acos-protocol` | — | working |
| verify (static + launch) | `acos verify` | `acos_verify` | **working for the counter core loop** (W180-A, `scenarios/v1-counter.yaml`, no Android boot); `hello-views` tap-after-type and `hello-jni` native symbols are blocked (`docs/V1.md`) |
| launch Activity / lifecycle | `acos run` | `acos_run` | working on `counter`/`hello-views`/`form-validation` (W159-A, W161-A); rotation/recreate not implemented |
| UI tree / text | `acos observe` | `acos_observe` | working (text/ids/classes); bounds offset vs ART is open (W179-A) |
| screenshot / frame | `acos capture` | `acos_capture` | working: fresh forced frame + stable hash for a static screen (W180-A `AgentSession::capture`) |
| tap / type / back / rotate | `acos action` | `acos_action` | tap + type work on `counter`/`hello-views`/`form-validation` (W161-A, W167-A, W180-A); back untested; rotate not implemented |
| assertions against the app | `acos assert` | `acos_assert` | working through `acos verify` (exact text, bounds, ordered logs, frame binding) |
| event stream | `acos events` | `acos_events` | `verify` writes `events.jsonl` with frame/tree/log events |
| reset / profile / destroy | `acos reset/profile/destroy` | `acos_reset/…` | reset/destroy work at the session level; `profile` reports the process peak RSS |
| capability report | `acos capability-report` | `acos_capability_report` | **not working yet** |

MCP availability is probed at runtime from `acos --help`, not hard-coded: as
soon as the CLI implements a command it appears as available. `acos_capabilities`
and `acos_help` are server tools. See `docs/MCP.md`.

---

## 10. Worked example — `counter`

```bash
# 1-2. build (adds counter.apk alongside the other fixtures)
tools/build-apps.sh

# 3. inspect
./target/release/acos inspect fixtures/build/apps/counter.apk --json
#    -> status pass, package dev.acos.fixtures.counter, launcher .MainActivity

# 4. capture ART reference (emulator must be running)
tools/oracle/emulator.sh start
tools/oracle/capture-app-reference.sh
tools/oracle/emulator.sh stop
#    -> fixtures/reference/counter/art-api34-x86_64/

# 5. validate the scenario
tools/guarded.sh --mem 4G --time 900 -- cargo test -p acos-protocol

# 6a. native verify — truthful partial today
./target/release/acos verify scenarios/counter.yaml --json
#    -> launch pass, wait_for unsupported (ui.tree), verdict unsupported

# 6b. reference replay — proves the assertions and negatives are honest
./target/release/acos verify scenarios/counter.yaml \
  --reference fixtures/reference/counter/art-api34-x86_64 --json
./target/release/acos verify scenarios/negative/counter-wrong-count.yaml \
  --reference fixtures/reference/counter/art-api34-x86_64 --json
#    -> positive stops at `tap` (input unsupported); negative verdict fail
```

---

## 11. API-level differences (Android 15 device vs API 34 emulator)

The oracle is an **API 34** (Android 14) `google_apis` x86_64 emulator. A real
phone may run **Android 15 (API 35)**. Differences that matter for these
fixtures:

| area | API 34 emulator (oracle) | Android 15 (API 35) |
|---|---|---|
| `EditText` empty text | uiautomator reports the **hint** as `text` | same, but Material/theme differences can change padding and therefore bounds |
| status bar / insets | 3-button nav, `dumpsys window displays` geometry as captured | gesture nav and edge-to-edge enforcement changes top/bottom insets → **bounds shift** |
| `AlertDialog` | framework theme, `android:id/button1` = `OK` | same ids, possible Material 3 restyle (size/position) |
| targetSdk 34 app on API 35 | n/a | forced edge-to-edge and predictive back may change layout/back behaviour |
| `ListView` rows | `android:id/text1`, height 126 px at 420 dpi | same id; row height depends on theme/font scale |
| log tags / lifecycle | as captured | identical order; Android 15 may add extra `onStop`/`onStart` around multi-window |

**Consequence:** texts and ids are stable across API levels; **pixel bounds
are not.** Scenarios that assert `bounds` are calibrated to the API 34 oracle
and must be re-captured (or bounds relaxed) before they are used as a gate on a
different API level.

**Physical-device spot check: not captured in this workstream.** The expected
`moto g35` (`adb` id `ZA222WGJPP`) was not present on `adb devices` during the
run (`adb devices` listed only `emulator-5554`). To capture it later:

```bash
adb -s ZA222WGJPP install -r -g fixtures/build/apps/counter.apk
adb -s ZA222WGJPP shell am start -W -n dev.acos.fixtures.counter/.MainActivity
adb -s ZA222WGJPP exec-out screencap -p > counter-physical.png
adb -s ZA222WGJPP shell uiautomator dump /sdcard/w.xml
adb -s ZA222WGJPP exec-out cat /sdcard/w.xml > counter-physical-ui.xml
```

Label the evidence `physical-device` and record `ro.build.version.sdk` (35) and
the build fingerprint; do **not** overwrite the API 34 `art-reference`
directories.

---

## 12. Host safety (ADR-0018, mandatory)

Every command that can build or execute guest code goes through
`tools/guarded.sh`:

```bash
tools/guarded.sh --mem 4G --time 900 -- cargo test -p acos-protocol
tools/guarded.sh --mem 4G --time 1200 -- tools/oracle/capture-app-reference.sh
tools/guarded.sh --mem 6G --time 1800 -- tools/build-apps.sh   # already self-wraps
```

The emulator is started/stopped directly with `tools/oracle/emulator.sh`
(it self-checks `MemAvailable > 16 GiB`, RAM ≤ 2048 MB, one instance, headless)
and is **always** shut down afterwards (`pgrep -af qemu` must be empty). Use a
private `CARGO_TARGET_DIR` when other agents are building concurrently, and
`source tools/agent-env.sh` first (§14) so the build goes through the shared
compiler cache.

---

## 13. Agent session loop (build → launch → observe → input → logs)

The tight loop an AI agent runs while developing an app. It uses the commands
from `docs/AGENT_API.md` §18; every step is one JSON envelope and a non-zero
exit on any non-`pass` status.

```bash
export CARGO_TARGET_DIR=$PWD/.cache/target-agent
export ACOS=$(cargo build -q -p acos-cli && echo target/debug/acos)

# 1. build the app (the existing fixture pipeline)
tools/build-apps.sh

# 2. launch and read the ladder. Exit 1 means a rung did not complete; read
#    result.reached_stage and error.details.stage, never "retry until green".
tools/guarded.sh --mem 4G --time 300 -- "$ACOS" launch \
  fixtures/build/apps/counter.apk --json | tee artifacts/launch.json

# 3. observe. Before the first frame this is `not_ready`; it is not a bug in
#    your app and it is not a pass.
tools/guarded.sh --mem 4G --time 300 -- "$ACOS" observe --apk \
  fixtures/build/apps/counter.apk --screenshot artifacts/frame.png --json

# 4. drive input only after a window exists (today the dispatcher answers
#    NoWindowAtPoint until WMS publishes a window).
tools/guarded.sh --mem 4G --time 300 -- "$ACOS" tap 120 340 --apk \
  fixtures/build/apps/counter.apk --json
tools/guarded.sh --mem 4G --time 300 -- "$ACOS" key back --json

# 5. read the guest log buffer (empty is valid; no fabricated lines).
tools/guarded.sh --mem 4G --time 300 -- "$ACOS" logs --apk \
  fixtures/build/apps/counter.apk --json

# 6. reset / close when the task state is polluted.
tools/guarded.sh --mem 4G --time 300 -- "$ACOS" session reset --json
tools/guarded.sh --mem 4G --time 300 -- "$ACOS" session close --json
```

Reading the result:

* `ok:true`/`status:pass` → the rung ran. Only then may the next command rely
  on it.
* `unsupported`/`UNSUPPORTED_BACKEND` → the engine cannot do it yet. Report the
  gap; do not rewrite the app or the expectation to hide it.
* `error`/`JAVA_EXCEPTION`/`VM_*` → the app or the engine threw. Read
  `error.details.stage` (the rung) and the stack top; this is not an assertion
  failure.
* `fail`/`ELEMENT_NOT_FOUND`/`ASSERTION_FAILED` → the app ran and a check did
  not hold. Read the linked frame/tree.

MCP: when the loop must keep one session across calls (an agent that inspects,
taps and observes in separate requests), run the default `acos-mcp` binary. Its
`SessionBackend` holds the session in-process; the CLI's session ends with the
command. See `docs/AGENT_API.md` §18.6.

---

## 14. Shared compiler cache (sccache) — source `tools/agent-env.sh` first

Every agent worktree on this host shares one compiler cache, so a worktree does
not recompile the crates another worktree already compiled. There is exactly one
place that sets it up:

```bash
source tools/agent-env.sh          # in the worktree, before any build
```

`BRIEF_COMMON` asks agents to create `.cache/env.sh` (a stable path, because
worktree names are not); make it a one-liner so there is nothing to keep in sync:

```bash
mkdir -p .cache && echo 'source tools/agent-env.sh' > .cache/env.sh
```

What it exports:

| variable | value | why |
|---|---|---|
| `RUSTC_WRAPPER` | `~/.cargo/bin/sccache` | the cache wrapper |
| `SCCACHE_DIR` | `/home/Qui/.cache/sccache` | one cache for the host (disk, LRU-evicted) |
| `SCCACHE_CACHE_SIZE` | `40G` | ~2x a full workspace build |
| `SCCACHE_BASEDIRS` | `/home/Qui/.t3/worktrees/acos:/home/Qui/Projects/acos/.cache:/home/Qui/Projects/acos` | these are the layouts worktrees are checked out in; they are interchangeable checkouts, so the same sources under any of them hash to one key |
| `CARGO_INCREMENTAL` | `0` | sccache cannot cache incremental compiles at all |
| `CARGO_PROFILE_{DEV,TEST,RELEASE}_DEBUG` | `0` | target dirs several times smaller (the brief's rule) |
| `ACOS_FONT_DIR`, `ACOS_VIEW_TEST_DIR`, `.cache/{boot/android-34,art,gaten}` symlinks | as `BRIEF_COMMON` | the ART/fixture test env |

It does **not** set `CARGO_TARGET_DIR`: keep one target directory per track
(§12) — the cache is what makes that cheap. It never sets `ACOS_BOOT_DIR` or
`XDG_RUNTIME_DIR`, and it never touches `ACOS_HOME` or `~/.local`.

Notes:

* The cache is shared by *all* agents on the host, so a crate compiled on one
  branch is reused on another as long as its sources and flags are identical.
  Cross-worktree hits are what the patch in `tools/sccache/` exists for; stock
  sccache only reuses artifacts inside one checkout.
* A crate that bakes an absolute path into its artifact (`env!("CARGO_MANIFEST_DIR")`
  — `acos-mcp` does) is never served another checkout's build: its key keeps the
  real path, so it is recompiled per worktree. That is intentional.
* `sccache --show-stats` reports hits/misses and the cache size;
  `sccache --zero-stats` resets the counters. If a build seems uncached, check
  that `$RUSTC_WRAPPER` is set in the shell that runs `cargo` and that
  `sccache --show-stats` shows the expected `Base directories`.
* Rebuild/reinstall the wrapper with `tools/sccache/build.sh` (see
  `tools/sccache/README.md` for the one patch it applies and why).
* Measurements (cold vs warm vs a second worktree, hit rates, cache size):
  `results/handoff/BUILDCACHE/README.md`.
