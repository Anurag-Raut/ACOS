# W132-A dogfood — agent-experience wave 4: a new 2-screen notes app

**Date:** 2026-11-11. **Worktree:** clean `.cache/wt-w132a` @ `1041c21`
("M2 reaches onResume on counter.apk … W124-A, W129-A"). **Binary:**
`.cache/target-w132a/debug/acos` (`CARGO_TARGET_DIR=.cache/target-w132a`,
`CARGO_PROFILE_DEV_DEBUG=0`, `tools/guarded.sh`, host RAM < 90 %).
**Scope:** act as an app developer with *only* the ACOS tool surface, build a
new 2-screen notes app outside the repo tree with the repo's Gradle-free
fixture toolchain, drive the full `build → acos launch → acos observe →
tap/text → verify` loop, record every break precisely, and fix the
tooling/UX/doc friction this wave owns.

The engine blockers found here (settings-provider `LIST_config`, the missing
first frame) are **not** fixed by this wave; they are named with owners in §6.

---

## 0. The new app

| | |
|---|---|
| Source | `/tmp/acos-dogfood-w132/notesapp/` (scratch, outside the repo) |
| Package | `dev.acos.dogfood.notes` |
| APK | `/tmp/acos-dogfood-w132/notesapp.apk` (12 838 bytes) |
| Build | `/tmp/acos-dogfood-w132/build.sh` — the exact aapt2 + javac + d8 + zipalign + apksigner pipeline of `tools/build-dogfood-apps.sh`, pointed at the scratch dir |
| Screen 1 | `MainActivity`: `TextView` title + `Button` "Add note" + `ListView id=notes_list` + empty `TextView`; adapter is an `ArrayAdapter` over the in-memory store; `onResume` refreshes |
| Screen 2 | `AddNoteActivity`: `EditText id=note_input` + `Button id=save_button`; Save appends to `NotesStore` and `finish()`es |
| Navigation | `startActivity(new Intent(this, AddNoteActivity.class))`; BACK returns to the list |
| State | `NotesStore` — a process-static `ArrayList<String>` (in memory, no disk) |

The app is a normal Android app: a `ListView` list screen and an `EditText`
add screen. Nothing was swapped for a "supported" widget to dodge an engine
gap.

---

## 1. Loop evidence and timings

All commands run with `ANDROID_HOME=/home/Qui/Android/Sdk`; the daemon steps use
`ACOS_RUNTIME_DIR=/tmp/acos-w132`; the one-shot steps set `ACOS_NO_DAEMON=1`.

| Step | Command | Wall | Result |
|---|---|---:|---|
| build APK | `/tmp/acos-dogfood-w132/build.sh` | **1.15 s** total (aapt2 2 ms, link 61 ms, javac 443 ms, d8 340 ms, sign+align 288 ms) | `notesapp.apk` 12 838 B |
| preflight | `acos doctor --json` | < 50 ms | `healthy`, 0 failures |
| inspect | `acos inspect …/notesapp.apk --json` | **3.4 ms** | `pass`, package `dev.acos.dogfood.notes`, launcher `…MainActivity` |
| launch (warm) | `acos launch …/notesapp.apk --json` | **3.32–3.36 s** | `unsupported`, reached `client_transaction`, blocked `activity_on_create` |
| launch (cold, first VM) | same, first run in a fresh process | **6.48–8.64 s** | same blocker |
| launch `counter.apk` (warm) | `acos launch fixtures/build/apps/counter.apk --json` | **3.41–3.48 s** | `unsupported`, reached **`on_resume`**, blocked `first_frame` |
| launch `counter.apk --keep` (daemon) | `acos launch … --keep --json` | 6.50 s (cold) | `session s_cli`, reached `on_resume` |
| observe tree (live) | `acos observe --tree --json` | **13.5 ms** | `pass`, 8-node `View` tree, `stage_reached: on_resume` |
| observe frame | `acos observe --json` | 35.5 ms | `unsupported` `UNSUPPORTED_BACKEND` `ui.frame` `not_ready` |
| tap by text | `acos tap --text Increment --json` | **26.4 ms** | `pass`, `accepted: true`, `state_changed: false` |
| tap by coords | `acos tap 199 383 --json` | 20 ms | `pass`, `accepted: true`, `state_changed: false` |
| type text | `acos session text --text hello --json` | 40.6 ms | `pass`, `text_path: "key-events"`, `state_changed: true` (no editor focused) |
| key | `acos key BACK --json` | 37.1 ms | `pass`, `accepted: true` |
| logs | `acos logs --json` | 40.0 ms | `pass`, 54 lines; `onCreate/onStart/onResume` present, no `increment value=1` |
| status | `acos session status --json` | 34.2 ms | `pass`, `launched: true`, `stage_reached: on_resume`, `published_frames: 0` |
| reset | `acos session reset --json` | 66.6 ms | `pass`; tree afterwards is `not_ready` again |

### Comparison with `BENCHMARKS.md` §9

The §9 numbers are VM-level micro-benchmarks; `acos launch` measures the whole
APK load + framework boot + lifecycle, so only the observe row is directly
comparable.

| Metric | W132 measured | `BENCHMARKS.md` §9 | Note |
|---|---:|---:|---|
| cold process → `Vm` ready | (not measured) | 16.43 ms p50 | §9.1 |
| framework boot → `attachApplication` | (not measured) | 13.60 ms p50 | §9.1 |
| `Vm::restore` from ACOSSNAP | (not measured) | 6.97 ms p50 | §9.1 |
| **full `acos launch` → `on_resume`** | **3.3–3.5 s warm / 6.5–8.6 s cold** | — | not a §9 row; W81-A recorded ~5.5 s at the `client_transaction` rung |
| `acos observe --tree` | **13.5 ms (live tree, 8 nodes)** | 6.10 ms p50 (`not_ready` path, spread 49 %) | §9.1; live-tree row is new |
| `acos observe` (frame) | 35.5 ms | — | `not_ready` path |
| `acos tap` | 26.4 ms | — | acked; no click lands (see W4-02) |
| MCP `tools/call acos_inspect` | (CLI: 3.4 ms) | 5 ms p50 (p95 11) | §9.1 |
| SessionPool reset-to-clean | 66.6 ms (`session reset`) | 2.08 ms (pool core-VM only) | different scope: app-scoped reset drops the window |

**Reading:** the lifecycle work (W124-A) moved the honest launch ceiling from
`client_transaction` to `on_resume`, and `observe --tree` now returns a real
tree in 13.5 ms — the loop is usable end-to-end for a `TextView`/`Button` app.
The remaining ~3.3 s is dominated by APK/framework boot, not by the per-step
tools (all ≤ 67 ms). The two noisy §9 rows are unchanged.

### What worked well

* `acos launch --keep` + separate-process `observe --tree`/`tap`/`text`/`logs`/
  `status`/`reset` all address one daemon session: the develop→run→observe→act
  loop is real across CLI processes.
* `acos observe --tree` at `on_resume` returns a genuine `View` hierarchy with
  `resource_id`/`bounds`/`clickable`/`focused`; node ids (`n5`, `n6`, …) are
  stable enough to script.
* `observe` before a frame is `unsupported` `not_ready` with
  `details.capability = "ui.frame"` and `stage_reached`, never an empty pass.
* `acos doctor --json` is healthy and fast.
* The scratch-dir build with the repo's toolchain worked first try (1.15 s);
  `acos inspect` confirmed the launcher.

---

## 2. The launch ladder for the notes app (the break)

`acos launch /tmp/acos-dogfood-w132/notesapp.apk --json`:

```
apk_parsed             ok
process_booted         ok
bind_application       ok
application_on_create  ok
client_transaction     ok
activity_on_create     blocked  UNSUPPORTED_JAVA_API
on_resume              not_reached
first_frame            not_reached
```

`status: unsupported`, `reached_stage: client_transaction`,
`error.details`:

```json
{
  "stage": "activity_on_create",
  "hint": "performLaunchActivity threw before Activity.onCreate",
  "root_cause": "java.lang.UnsupportedOperationException: unsupported Android API: android.content.IContentProvider code 0 (LIST_config)",
  "api": "android.content.IContentProvider;->LIST_config",
  "at": "android.content.IContentProvider.LIST_config",
  "classes_loaded": 2172
}
```

The message is a framework `InflateException: … Error inflating class
android.widget.ListView`, caused by
`android.content.IContentProvider code 0 (LIST_config)`.

**The list screen never creates, so no window, tree, tap or text is reachable
for the app as specified.** The same wall stops every list/text fixture:
`list-scroll` (`ListView`) fails identically, and `hello-views` (`EditText`)
fails one widget deeper with a deterministic parcel error (see W4-01b).

---

## 3. Friction table

Severity: **H** blocks an agent, **M** wastes its time / misleads it, **L**
polish. "Owner" is the crate/subsystem, per the wave ownership map.

| # | Friction (what the agent saw) | Sev | Status | Owner / fix |
|---|---|---|---|---|
| **W4-01a** | A `ListView`/`EditText` activity cannot start: `activity_on_create` blocks with `InflateException` whose real cause is `android.content.IContentProvider code 0 (LIST_config)` (the settings provider does not answer the `LIST_config` call). No themed list/text app can launch. | **H** | **open** | engine — settings/provider (`crates/acos-services/src/provider.rs` + `crates/acos-runtime/src/transport.rs`/`settings_java.rs`). Documented by W124-A in `results/m2/M2_LOG.md`. Not this wave. |
| **W4-01b** | `hello-views` (`EditText`) on current HEAD fails with `java.lang.IndexOutOfBoundsException: index=-2108489900 out of bounds (limit=28431696, nb=4)` at `LayoutInflater.createView`, deterministic over 3 runs. This is a **new** cause vs the release binary that predates W125-A (which failed on `LIST_config`). | **H** | **open** | engine — parcel/settings reply (`W125-A` parcel codec; last commit `10cf96b`). Needs triage: the huge negative index / 27 MB limit points at a misdecoded `MemoryIntArray`/provider reply. |
| **W4-02** | `first_frame` is `unsupported` (`ui.frame`, `not_ready`) even after `on_resume` ran. `acos observe` (frame) fails and a `tap` is acked but the click does not land (`state_changed: false`, no `increment value=1` log). The docs did not say input is a no-op before a frame. | **H** | **open** (docs fixed) | engine — render/handoff, W122-A/W129-A (per `M2_LOG.md`: "the app still needs a published frame for the click to land"). |
| **W4-03** | For a wrapped `InflateException` the `UNSUPPORTED_*` hint says *"Read `details.api`/`details.at`"*, but those fields were **absent**, and the true blocker was buried ~26 KB / 3 `caused by:` levels deep. | **M** | **fixed** | `crates/acos-runtime/src/session.rs`: `blocker_for` now surfaces `details.root_cause` (deepest `caused by:`) and parses `unsupported Android API: <iface> code <n> (<method>)` into `details.api`/`details.at`. Unit test `buried_java_cause_is_surfaced_and_parsed`. |
| **W4-04** | `acos tap --text Increment` / `acos tap 199 383` returns `status: pass`, `accepted: true` before a frame is published, but the button's `OnClickListener` never fires. `state_changed: false` is the only signal. | **M** | **open** (docs fixed) | tooling follow-up: a `details.note` when `state_changed == false` and `published_frames == 0` would make the no-op explicit (owner `acos-runtime`/`acos-cli`). Engine cause is W4-02. |
| **W4-05** | `acos session text --text hello` on `counter` (focused view is a `Button`, no editor) returns `status: pass`, `text_path: "key-events"`, `state_changed: true`. An agent can read that as "hello was typed"; it was not. | **M** | **open** (docs fixed) | tooling follow-up: report the focused class / a `note` when the fallback fires on a non-editor. Docs now say to read `text_path`. |
| **W4-06** | CLI `acos tap --x 199 --y 383` is rejected (`unexpected argument '--x'`); coordinates are positional (`acos tap 199 383`), while MCP `acos_tap` takes `{x, y}`. The quickstart only showed the MCP form. | **L** | **fixed (docs)** | `docs/AGENT_QUICKSTART.md` §2.6 now states the CLI form. |
| **W4-07** | `docs/AGENT_QUICKSTART.md` §6 "Known state" still claimed the launch stops at `VectorDrawable.nCreateGroup` (fixed) and did not mention the `on_resume` / `first_frame` / `LIST_config` state. | **M** | **fixed (docs)** | `docs/AGENT_QUICKSTART.md` §2.3/§2.6/§5/§6 rewritten to the `1041c21` state. |
| **W4-08** | `acos doctor` is `healthy` but says nothing about the launch ceiling; a fresh agent plans a list/text app that cannot start. | **L** | **open** | docs/UX: the quickstart now carries the ceiling; a doctor `note` is optional. |

---

## 4. Tooling/doc changes in this wave

Worktree `.cache/wt-w132a` @ `1041c21`, patch `git apply --check` clean against
the main tree.

| File | Change |
|---|---|
| `crates/acos-runtime/src/session.rs` | `blocker_for` adds `details.root_cause` (deepest `caused by:`) for `UNSUPPORTED_*`/`JAVA_EXCEPTION`, and `parse_unsupported_android_api` fills `details.api`/`details.at` from the `unsupported Android API: <iface> code <n> (<method>)` form. New unit test. |
| `docs/AGENT_QUICKSTART.md` | §2.3 documents the current ceiling; §2.6 documents the CLI positional tap and the `text_path`/`state_changed` caveats; §5 documents `details.root_cause`; §6 "Known state" rewritten to `1041c21`. |

No engine behaviour changed; the launch ladder and timings are identical
before/after the patch (the new fields only make the existing error legible).

---

## 5. Reproduce

```bash
export ANDROID_HOME=/home/Qui/Android/Sdk
ACOS=/home/Qui/Projects/acos/.cache/target-w132a/debug/acos

# build the scratch app (same pipeline as tools/build-dogfood-apps.sh)
/tmp/acos-dogfood-w132/build.sh
$ACOS inspect /tmp/acos-dogfood-w132/notesapp.apk --json

# W4-01: the notes app cannot create its list screen
ACOS_RUNTIME_DIR=/tmp/acos-w132d ACOS_NO_DAEMON=1 \
  $ACOS launch /tmp/acos-dogfood-w132/notesapp.apk --json | \
  python3 -c 'import json,sys; print(json.load(sys.stdin)["error"]["details"])'

# the loop that does work today: counter reaches on_resume + live tree
export ACOS_RUNTIME_DIR=/tmp/acos-w132d
$ACOS launch fixtures/build/apps/counter.apk --keep --json
$ACOS observe --tree --json          # pass, 8 nodes, stage_reached=on_resume
$ACOS tap --text Increment --json    # pass, accepted, state_changed=false (W4-02)
$ACOS observe --json                 # unsupported, ui.frame not_ready
$ACOS session text --text hello --json   # pass, text_path=key-events (W4-05)
$ACOS logs --json
$ACOS session status --json
$ACOS session reset --json
```

---

## 6. Engine blockers and owners

| Blocker | Exact evidence | Owner |
|---|---|---|
| `android.content.IContentProvider` `LIST_config` unsupported during `LayoutInflater.createView` (`ListView`; also the release-binary cause for `EditText`) | `results/m2/M2_LOG.md`; `acos launch list-scroll.apk` / notes app: `root_cause = java.lang.UnsupportedOperationException: unsupported Android API: android.content.IContentProvider code 0 (LIST_config)` | settings/provider — `crates/acos-services/src/provider.rs`, `crates/acos-runtime/src/{transport,settings_java}.rs` (W125-A-adjacent; not in W132 scope) |
| `IndexOutOfBoundsException index=-2108489900 out of bounds (limit=28431696, nb=4)` during `EditText` inflate on `1041c21` | `acos launch hello-views.apk` ×3, deterministic; `root_cause` in the envelope | parcel/settings reply — `W125-A` parcel codec (`10cf96b`) |
| No published frame (`ui.frame` `not_ready`) after `on_resume`, so `acos observe` fails and taps do not land | `acos observe`/`acos tap` on `counter`; `results/m2/M2_LOG.md` "first_frame is W122-A's … scope" | render/handoff — W122-A / W129-A (render harness) |
| `VectorDrawable.nCreateGroup` | **fixed** at `1041c21`; docs updated | W68-A (closed) |

---

## 7. Acceptance

* `WAVE4.md` complete (this file) — friction table with minimal repros, exact
  errors, severities and owners.
* `cargo test -p acos-runtime --lib` — **21 passed / 0 failed** (incl. the new
  `session::tests::buried_java_cause_is_surfaced_and_parsed`).
* `cargo clippy -p acos-runtime -p acos-cli -p acos-mcp --all-targets --no-deps -- -D warnings` — clean.
* `cargo check --workspace --all-targets` — clean.
* Patch `git apply --check` clean against the main tree; applied to the working
  tree, **not committed**.
* Scratch dir `/tmp/acos-dogfood-w132` deleted after this report.
