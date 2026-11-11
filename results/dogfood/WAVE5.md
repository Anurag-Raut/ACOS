# W159-A dogfood — agent-experience wave 5: the counter / hello-views / form-validation loop

**Date:** 2026-11-11. **Worktree:** clean `.cache/wt-w159a` @ `80cb4f5`
("Parcelable codec: ServiceInfo/InstrumentationInfo/PermissionInfo/ResolveInfo
Full; PMS getServiceInfo/getProviderInfo manifest-backed (W155-A)"). **Binary:**
`.cache/target-w159a/debug/acos` (`CARGO_TARGET_DIR=.cache/target-w159a`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_BUILD_JOBS=2`, `tools/guarded.sh`,
`ACOS_NO_BOOT_CACHE=1` for every launch, host RAM < 90 %).
**Scope:** act as an app developer with *only* the ACOS tool surface, run the
full `launch → observe --tree → observe --frame → tap --text → type →
re-observe → parity/frame-diff → measure` loop against the fixtures that reach
`first_frame`, record every friction with the exact command and output, then fix
the top three small/self-contained ones. Engine blockers are recorded with
owners, not fixed here.

> **Build note.** Committed HEAD `80cb4f5` did not compile from a clean
> worktree: `crates/acos-vm/src/javabridge.rs` calls
> `crate::natives::fwk_annotations::{mirror_method_id, make_method}` but
> `natives/mod.rs` declared `mod fwk_annotations;` (private) and `mod
> native_mem;`. The in-flight tree already had the one-line visibility fix
> (`pub(crate) mod …`); it is required to build and is **not** counted as one of
> the three friction fixes.

---

## 1. Fixtures that reach `first_frame`

| Fixture | `status` | `reached_stage` | launch wall | Notes |
|---|---|---|---:|---|
| `counter.apk` | pass | `first_frame` | 7.07–7.11 s (cold daemon) | `count=0`, `Increment`, `Reset` in the tree |
| `hello-views.apk` | pass | `first_frame` | 8.15 s (cold daemon) | `EditText input` + `Button Greet` + `result: clicks: 0` |
| `form-validation.apk` | pass | `first_frame` | 8.24 s (cold daemon) | `EditText name`/`email` + `Button Submit` + `error` |

All three are driven from the same daemon via `--keep`; each run used
`ACOS_NO_BOOT_CACHE=1` (the warm boot-image path is W154-A's area and is not
judged here).

---

## 2. Loop evidence and timings

`ANDROID_HOME=/home/Qui/Android/Sdk`, `ACOS_RUNTIME_DIR=/tmp/acos-w159a`,
`ACOS_NO_BOOT_CACHE=1`; one-shot rows set `ACOS_NO_DAEMON=1`.

| Step | Command | Wall | Result |
|---|---|---:|---|
| preflight | `acos doctor --json` | 71 ms | `healthy`, 0 failures |
| inspect | `acos inspect fixtures/build/apps/counter.apk --json` | 3 ms | `pass`, package `dev.acos.fixtures.counter` |
| launch counter | `acos launch …/counter.apk --keep --json` | **7.07 s** | `pass`, `reached_stage=first_frame`, `session s_cli` |
| observe tree | `acos observe --tree --json` | **54 ms / 26 ms** | `pass`, 8→12-node `View` tree, `count=0`, `Increment`, `Reset` |
| observe frame | `acos observe --json` | **14 ms** | `pass`, `result.frame.frame_seq=1`, `published_frames=1` |
| screenshot (daemon) | `acos observe --screenshot /tmp/counter-daemon.png --json` | **345 ms** | after fix: `result.screenshot {bytes: 10371258}`, `ui.screenshot`; file written |
| screenshot (one-shot) | `acos observe --apk … --screenshot /tmp/counter-one-shot.png --json` | 7.51 s | `result.screenshot {bytes: 10371258}` |
| tap text | `acos tap --text Increment --json` | **39–76 ms** | `pass`, `accepted:true`, **`state_changed:false`** (W5-02) |
| tap coords | `acos tap 199 383 --json` | 20 ms | same |
| type text | `acos session text --text hello --json` | 41–58 ms | `pass`, `text_path:"input-connection"`, tree shows `input='hello'` |
| logs | `acos logs --json` | 36 ms | `pass`, 56–57 records |
| status | `acos session status --json` | 34 ms | `pass`, `launched:true`, `stage_reached:first_frame` |
| parity (recorded) | `acos parity --recorded counter --acos-tree … --json` | 3 ms | 12 ACOS vs 17 device nodes, classified diffs |
| frame diff | `acos frame-diff fixtures/reference/counter/art-api34-x86_64/screenshot_initial.png /tmp/counter-one-shot.png --json` | 2.87 s | `pass`, global SSIM 0.413, differing fraction 0.203 |
| launch hello-views | `acos launch …/hello-views.apk --keep --json` | 8.15 s | `pass`, `first_frame` |
| type hello-views | `acos session text --text hello --json` | 49 ms | `input-connection`; tree `input='hello'` |
| tap Greet | `acos tap --text Greet --json` | 39 ms | `accepted:true, state_changed:false`; `result` stays `clicks: 0` |
| frame diff hello-views | `acos frame-diff …/hello-views/art-api34-x86_64/screenshot_initial.png /tmp/hello-views-one-shot.png --json` | 2.83 s | `pass`, SSIM 0.413 |
| launch form-validation | `acos launch …/form-validation.apk --keep --json` | 8.24 s | `pass`, `first_frame` |
| tap Submit (empty) | `acos tap --text Submit --json` | 109 ms | `accepted:true, state_changed:false`; `error` stays `''` |
| tap email (coords) | `acos tap 500 818 --json` | 20 ms | `accepted:true, state_changed:false`; focus stays on `name` |

**Reading.** The per-step tools are all tens of milliseconds; the cost is the
~7–8 s framework boot, and the honest ceiling is `first_frame` for all three
fixtures. The two real breaks in the tool surface are the silent screenshot
no-op (fixed) and the tap that never lands (engine, W152-A).

---

## 3. Friction table (ranked by impact on iteration speed)

Severity: **H** blocks/misleads the agent, **M** wastes its time, **L** polish.
"Owner" is the crate/subsystem; `fixed` = changed in this wave.

| # | Friction (what the agent saw) | Sev | Status | Owner / fix |
|---|---|---|---|---|
| **W5-01** | `acos observe --screenshot <path>` on a daemon-held session returned `status:"pass"` with **no file and no evidence**. `call_observe` hard-coded `session.observe(None)` and ignored the `screenshot` arg, so an agent could burn an iteration believing it had frame evidence. | **H** | **fixed** | `crates/acos-mcp/src/session_backend.rs` (`call_observe`): pass the path to `session.observe`, report `result.screenshot` and the `ui.screenshot` capability. |
| **W5-02** | A tap never lands even with a real `first_frame`: `acos tap --text Increment` → `accepted:true, state_changed:false`, `count` stays `0`; `form-validation` Submit is a no-op and tapping the `EditText` by coords does not move focus, so the form cannot be driven at all. | **H** | **open** | engine — counter tap/SurfaceControl/WMS (**W152-A**). Not fixed here. |
| **W5-03** | An accepted-but-no-op tap/type carried no reason: `{"accepted":true,"state_changed":false}`. An agent reads `status:"pass"` + `accepted:true` as "it worked". | **H** | **fixed** | `session_backend.rs` (`input_envelope`) + `crates/acos-cli/src/session.rs` (`finish_input`): add `result.note` for `accepted && !state_changed`, and a separate note for the `key-events` fallback. |
| **W5-04** | `acos parity --recorded hello-views` (no phone capture for that fixture) failed with `recorded device snapshot … is unreadable: No such file or directory`, which reads as file corruption and gives no next step. | **M** | **fixed** | `crates/acos-cli/src/parity.rs` (`recorded_parity`): distinguish a missing app capture from an unreadable file and list `available recorded apps: …`. Test added. |
| **W5-05** | `acos logs` returns the raw buffer with no tag/priority filter. On `hello-views` the framework `W ArrayMap: New hash … is before end of array hash` emits a **~40-line `java.lang.RuntimeException` stack trace 6×** and drowns the app's own `AcosFixture: onCreate/onStart/onResume` lines. The agent must fetch JSON and filter client-side. | **M** | open | `acos-cli`/`acos-mcp` logs surface: consider `--tag`/`--min-priority`/`--grep` or a default "app package tags first" ordering. |
| **W5-06** | The `observe --tree` node exposes `resource_id`, but `tap` only accepts `--text` or `x y` — there is no `--id`/`{id}` selector. For an empty `EditText` the only path is to compute the centre from `bounds`; combined with W5-02 the form is unreachable. | **M** | open | `acos-cli`/`acos-mcp`/`acos-runtime` action surface (the `Selector` type already supports `by_id`). |
| **W5-07** | Typing works (`input-connection`, tree shows `input='hello'`) but the `Greet`/`Submit` click never fires, so the "type → button → asserted result" loop cannot complete even though every individual tool call is a `pass`. | **M** | open | engine — same root cause as W5-02 (W152-A). |
| **W5-08** | The `W ArrayMap: New hash … is before end of array hash …` warning carries a `java.lang.RuntimeException: here` raised inside `android.util.ArrayMap.append` while decoding a `Bundle`/`ArrayMap` (`Settings$NameValueCache.getStringForUser`, `FeatureFlagUtils.isEnabled`, `TextView.makeSingleLayout`). It looks like a crash and signals a misdecoded parcel. | **M** | open | engine — parcel/Bundle codec (W125-A/W155-A area; not on the avoid list). Recorded. |
| **W5-09** | `acos observe --frame` is rejected: `error: unexpected argument '--frame' found`. "Frame" is the default `observe` result, and the wave vocabulary says `observe --frame`. | **L** | open | `acos-cli` `ObserveArgs`: accept `--frame` as the explicit frame selector. |
| **W5-10** | `acos session observe` has no `--screenshot`, while top-level `acos observe` does; the two entry points to the same session diverge. | **L** | open | `acos-cli/src/session.rs` (`SessionObserveArgs`). |
| **W5-11** | `acos observe --json` reports the frame at `result.frame`, but `docs/AGENT_API.md` §2 describes an optional top-level `frame` object. | **L** | open | contract/CLI: either set the envelope `frame` field or note the CLI shape. |
| **W5-12** | There is no framework-app frame-parity preset: `parity --flutter-frame` exists only for Flutter; for `counter`/`hello-views` the agent must know to call `acos frame-diff <art-reference>/screenshot_initial.png <acos.png>`. | **L** | open | docs/UX: a `parity --frame <app> <png>` preset would name the ART reference automatically. |

### What worked well

* `counter`, `hello-views` and `form-validation` all reach a real `first_frame`
  in ~7–8 s.
* `observe --tree` returns a genuine hierarchy in 26–54 ms with
  `resource_id`/`text`/`bounds`/`clickable`/`focused` — enough to script.
* `acos session text` uses the real framework `InputConnection.commitText`:
  `text_path:"input-connection"` and the re-observed `EditText` shows `hello`.
* `launch --keep` + separate-process `observe`/`tap`/`text`/`logs`/`status` all
  address one daemon session.
* `parity --recorded` (3 ms) and `frame-diff` (2.8 s) are fast and structured.
* Engine failures are still honest: no false `pass`, and the per-step tools
  return structured errors.

---

## 4. Fixes in this wave

Worktree `.cache/wt-w159a` @ `80cb4f5`; `git apply --check` clean against the
main tree.

| File | Change |
|---|---|
| `crates/acos-mcp/src/session_backend.rs` | `call_observe` honours the `screenshot` path (daemon/MCP path) instead of dropping it, returns `result.screenshot` + `ui.screenshot`, and reports `screenshot_error` when no frame exists. `input_envelope` adds `result.note` for `accepted && !state_changed` and for the `key-events` text fallback. New test `a_no_op_action_carries_a_note`. |
| `crates/acos-cli/src/session.rs` | `finish_input` carries the same `note` for the one-shot CLI path. |
| `crates/acos-cli/src/parity.rs` | `recorded_parity` distinguishes "app has no recorded capture" from "snapshot file unreadable" and lists available recorded apps. New test `missing_recorded_app_names_the_available_captures`. |
| `crates/acos-vm/src/natives/mod.rs` | Build unblock (required): `pub(crate)` on `fwk_annotations` and `native_mem`, matching the one-line fix already in the in-flight tree. Not counted as a friction fix. |

No engine behaviour changed; the launch ladders and per-step timings are the
same before/after.

---

## 5. Reproduce

```bash
export ANDROID_HOME=/home/Qui/Android/Sdk ACOS_NO_BOOT_CACHE=1
export ACOS_RUNTIME_DIR=/tmp/acos-w159a
ACOS=/home/Qui/Projects/acos/.cache/target-w159a/debug/acos

$ACOS doctor --json
$ACOS inspect fixtures/build/apps/counter.apk --json

# W5-01: daemon screenshot (fixed — file + result.screenshot now appear)
$ACOS launch fixtures/build/apps/counter.apk --keep --json
$ACOS observe --tree --json
$ACOS observe --screenshot /tmp/counter-daemon.png --json
ls -l /tmp/counter-daemon.png

# W5-03: the no-op note
$ACOS tap --text Increment --json      # result.note explains state_changed=false

# W5-04: missing recorded capture (fixed message)
$ACOS parity --recorded hello-views --json

# text lands, tap does not (W5-02/W5-07)
$ACOS launch fixtures/build/apps/hello-views.apk --keep --json
$ACOS session text --text hello --json
$ACOS observe --tree --json
$ACOS tap --text Greet --json
$ACOS observe --tree --json
$ACOS logs --json

# frame parity
$ACOS parity --recorded counter --acos-tree /tmp/counter-tree.json --json
$ACOS frame-diff fixtures/reference/counter/art-api34-x86_64/screenshot_initial.png /tmp/counter-daemon.png --json
```

---

## 6. Acceptance

All run from the clean worktree with `CARGO_TARGET_DIR=.cache/target-w159a`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_BUILD_JOBS=2`, `tools/guarded.sh`; the raw
log is `results/dogfood/WAVE5-acceptance.txt`.

| Check | Result |
|---|---|
| `cargo test -p acos-mcp -p acos-cli` | green — every suite `ok` (17+6+4+2+1+10+2+39+1+1+1+1+20+3 tests), `TEST_EXIT=0` |
| `cargo clippy -p acos-mcp -p acos-cli --all-targets --no-deps -- -D warnings` | clean, `CLIPPY_EXIT=0` |
| `cargo check --workspace --all-targets` | green, `CHECK_EXIT=0` |
| `ACOS_BIN=…/debug/acos tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported** |

Worktree-only test setup (matches W3-06): `fixtures/build/{apps,dexprogs,jni}`
is **copied** into the worktree (not symlinked, so `validate_path` still sees the
paths under the allowed root) and `target/` is a real directory holding only
`debug/`→the external target plus `acos-mcp-artifacts/`; a bare `target`
symlink made the MCP `acos_verify` test fail with `UNSAFE_PATH` (status
`error`, not `fail`).

### Copy-back

`git diff` of the four source/docs files plus the two new `results/dogfood`
files is at `.cache/w159a.patch`; `git apply --check` against the main tree is
clean (including the new files). `crates/acos-vm/src/natives/mod.rs` is
intentionally excluded: the main tree already carries the same `pub(crate)`
build unblock. Nothing is committed.
