# V3 — the interactive session loop on the CLI/engine side (verify → engine seam)

**Status: implemented and verified against a scripted live session; the real ART
session on this host crashes before its ready line, so no real-ART pass is claimed.**
Owner: V3 implementer agent, 2026-11-11 15:30 → 16:00 UTC (`date -u`), ~30 min
(finished before the 45-minute status checkpoint; no 60-minute overrun).
Files written (all inside this agent's ownership):

- `crates/acos-cli/src/engine/{mod,art,fake,rust_vm}.rs`
- `crates/acos-cli/tests/verify_engine_art.rs` (new)
- `scenarios/v2-art-counter.yaml` (comments only: the enforced invariants are named)
- `results/art-runtime/v3-session-loop/**` (new evidence), this file
- **not touched**: `crates/acos-art/**` (W4), `crates/acos-verify/**`,
  `crates/acos-protocol/**`, `crates/acos-cli/src/verify.rs` (V2's), every other
  agent's files. No commit.

> Naming: `results/art-runtime/V3.md` already holds the vsync work (commit
> `1b5c3a6`). This is a different workstream, so it is written here as
> `V3-session-loop.md` to avoid overwriting committed evidence.

## 0. Acceptance

| requirement | result |
|---|---|
| `cargo test -p acos-cli --test verify_engine_art` | **PASS** 3/3 (real child process, live path, teardown) |
| `cargo test -p acos-cli --bin acos engine` | **PASS** 19/19 |
| `cargo test -p acos-cli --test verify` | **PASS** 2/2 (Rust-VM default path unchanged) |
| `cargo clippy -p acos-cli --all-targets` | **PASS**, no warnings |
| `acos verify --engine art` against a scripted session, `scenarios/v2-art-counter.yaml` | **PASS**, `engine: art`, `mode: live`, 12/12 steps, frames 4→5→6→8 |
| `acos verify --engine art` on this host (real `acos-art`) | honest `INTERNAL` + `LIVE_FRAME_UNAVAILABLE: … session ended before its ready line … outcome=Crash`; **no pass, no fallback** |
| teardown | the session's whole process group is killed and scanned; a leaked guest helper is dead after the run |
| pre-existing red tests | `tests/session.rs` (2) and `tests/negative_verify.rs` (1) fail in this shared tree; proved unrelated by re-running with these four engine files reverted (§6) |

Every command ran under `tools/guarded.sh --mem 6G`. Scratch in `.cache/` and
`/tmp`, never `/tmp` for ART data; no ART run outside `tools/guarded.sh --mem 4G`.

## 1. The backend trait (`engine/mod.rs`)

`SessionBackend` now names the whole interactive loop and makes three rules part
of the contract rather than caller convention:

| operation | contract |
|---|---|
| `launch(apk)` | starts the session, returns the first frame when one exists |
| `tree()` | traverses now, returns the live tree of the **new** frame |
| `frame()` | one bound observation: tree + PNG of the same completed traversal |
| `last_frame()` | the bound frame the last command completed, **without issuing a command** |
| `last_frame_seq()` | highest completed `frame_seq` the session has seen (a tree-less capture can pass the bound tree) |
| `screenshot()` | a **fresh** capture command; returns `FrameEvidence` or `STALE_FRAME`; never the cached PNG |
| `capture()` | a fresh capture bundled with the tree; default fails when the sequence did not advance |
| `tap(x,y)`, `tap_text(t)`, `type_text`, `key`, `reset` | deliver one action |
| `close()` | send `close`, reap, kill any group survivor, and **verify** the tree is empty |
| `teardown()` | close + `TeardownReport { process_group, survivors, verified }` |

`FrameEvidence` is the anti-staleness token: `frame_seq`, `previous_seq`,
`captured_at_ms`, `captured_at` (RFC 3339 UTC), `md5`, `png`. A capture is fresh
exactly when `frame_seq > previous_seq` (`is_fresh`); `label()` gives
`frame 4 after 3 at 2026-11-11T15:51:06.123Z` for logs and step detail.
The executor's `FrameMeta.produced_at` is stamped from this host capture time, so
a step's evidence time is the time its pixels were complete.

### Honest errors, never a pass

| engine code | protocol code | when |
|---|---|---|
| `LIVE_FRAME_UNAVAILABLE` | `NO_FRESH_FRAME` | no frame exists yet, a reply carried no `frame_seq`/PNG/tree, the session died before its ready line, the engine has no capture command |
| `STALE_FRAME` | `NO_FRESH_FRAME` | a fresh capture or an input action completed frame `N` while the previous frame was `>= N`; the frame is refused instead of being re-labelled fresh |
| `PROCESS_TREE_DIRTY` | `INTERNAL` | processes are still alive in the session's group after teardown (the error lists the pids) |

Backend startup failures keep `UNSUPPORTED_BACKEND` + `engine.art.session` (V2
rule). Missing commands keep their capability name (`input.type_text`, `logs`, …).
Preview (`mode: preview`) is still downgraded to `unsupported` by `verify.rs`.

### Why `observe()` no longer traverses

The executor binds a pure assertion to the frame the preceding action completed
(`observation.frame.frame_seq == bound_frame_seq`). The real guest publishes a
**new** frame for every command, so an `observe()` that issued `tree` would return
`N+1` and every `assert_text` after `wait_for`/`capture`/`tap` would be reported
as a frame-binding failure. `EngineBackend::observe()` therefore returns
`last_frame()` (the bound observation) and never invents a newer traversal;
`capture` is the frame-advancing observation step. This is also what makes the
scenario's `assert_text` after `tap` describe the tap's own frame.

### Action freshness (and a real bug this found)

`EngineBackend::input()` records the sequence **after** selector resolution and
**before** the action, then requires the action's own completed frame to be
newer. The first version recorded it before resolution; the scripted-session test
caught that an id-selector tap traverses once (to find the node) and that
traversal masked a tap which completed no frame at all (verdict passed with
`count` still `0`). The integration test `a_tap_that_completes_no_new_frame_is_refused`
is the regression; the engine test covers both selector styles.

## 2. `ArtSession` (`engine/art.rs`)

- `screenshot()` issues `{"cmd":"screenshot"}`, requires `frame_seq` and `png`,
  checks `seq > seen_seq`, reads ≤64 MiB of PNG, and returns `FrameEvidence`
  stamped when the bytes were read. A tree-less reply advances `seen_seq` but
  never rebinds the cached tree (a tree cannot be attached to newer pixels).
- Every reply that carries a tree replaces the bound cached frame with its
  seq/md5/png/tree and capture time; `last_frame()` returns exactly that pair.
- `close()`: `close` command → wait ≤10 s → kill the group → **kill any remaining
  group members even after the leader exited** → scan `/proc/*/stat` field 5 for
  the group → 2 s grace → `PROCESS_TREE_DIRTY` with the survivor pids. This is the
  process-tree guarantee at the CLI layer (the `EngineBackend` drop calls it).
- Startup: the ready line's failure keeps the capability error; a session that
  dies/timeouts before the ready line reports `LIVE_FRAME_UNAVAILABLE` with the
  child's stderr tail (see the real-ART case in §4).

## 3. Tests

In-crate (`engine/mod.rs`, `engine/art.rs`, `engine/fake.rs`):

| test | proves |
|---|---|
| `counter_scenario_passes_through_the_art_backend_path_live` | the scenario runs through the session seam, live pass, real `tap` reached the backend |
| `preview_backend_never_yields_a_pass` | preview is downgraded to `unsupported` |
| `unsupported_step_names_the_missing_capability_and_never_falls_back` | `type` stops with `input.type_text`; the Rust-VM probe records 0 calls |
| `fresh_screenshot_carries_a_new_sequence_and_a_capture_time` | two captures: increasing `frame_seq`, `previous_seq` chaining, RFC 3339 time, non-empty PNG |
| `stale_tap_is_rejected_as_no_fresh_frame` | a tap with no new frame fails `NO_FRESH_FRAME` + `STALE_FRAME`, for text **and** id selectors |
| `stale_capture_is_rejected_as_no_fresh_frame` | a capture that reuses the previous frame fails, never passes |
| `no_live_frame_is_an_honest_error_never_a_pass` | `observe`/`capture` return `LIVE_FRAME_UNAVAILABLE`, non-pass |
| `the_whole_scenario_fails_honestly_when_no_live_frame_arrives` | the full scenario cannot pass without a live frame |
| `a_pure_observation_does_not_invent_a_newer_frame` | repeated `observe` keeps the seq, tree frame == observation frame, no traversal issued |
| `teardown_says_whether_the_process_tree_is_clean` | `TeardownReport` is verified+clean, idempotent |
| `teardown_scan_sees_the_whole_process_group_and_ends_clean` | real `/bin/sh` + grandchild in their own group: scan sees both, SIGKILL leaves no survivor |
| `art_session_capabilities_report_the_protocol_gaps` | launch/tree/screenshot/tap/tap_text/key/reset/close present; type_text/logs/environment/crash_reports named missing |

CLI integration (`tests/verify_engine_art.rs`, a scripted `$ACOS_ART_BIN` that
speaks the session protocol and forks a `sleep` helper it never reaps):

| test | proves |
|---|---|
| `art_counter_scenario_passes_on_a_live_session_and_tears_the_tree_down` | `acos verify --engine art`: envelope `pass`, verdict `pass/engine art/mode live`, 12 steps, tap×2, capture seq > tap seq, second tap seq > capture seq, and the forked helper is gone |
| `a_tap_that_completes_no_new_frame_is_refused` | CLI fails at the `tap` step with `NO_FRESH_FRAME` + `STALE_FRAME`; teardown still cleans the tree |
| `a_session_without_a_live_frame_fails_honestly` | a valid ready line with no frame ⇒ step failure `NO_FRESH_FRAME` + `LIVE_FRAME_UNAVAILABLE`, never `pass`, never `preview` |

## 4. Evidence

`results/art-runtime/v3-session-loop/` — a real CLI run
(`target/debug/acos verify --engine art … --json`) against the scripted session:

- `verdict.json`: `verdict: pass`, `engine: art`, `mode: live`,
  `unsupported_steps: []`; step frames `launch -, wait_for 2, assert 2, assert 2,
  tap 4, assert 4, capture 5, reset -, wait_for 6, assert 6, tap 8, assert 8`
  (the `point_for` traversal is 3/7; the tap's own frame is 4/8).
- `envelope.json`, `engine.json`, `repro.sh`, `steps/*`, `frames/*`, `tree/*`.
- Helper pid `2428384` was absent from `/proc` after the run (teardown).
- `host-checks/real-art-session-crash-stderr.txt`: the real `acos-art session`
  on this host reaches `onCreate → onStart → onResume`, `Activity.attach()`,
  `dispatchApplyWindowInsets(...)`, then exits with `outcome=Crash` in **339 ms**
  (`session=s…86408737 commands=0 frames=0`). That crash is the live-frame
  blocker and belongs to the window/framework workstream, not this file set.
- `host-checks/real-art-host.json`: `acos verify --engine art` on that host is
  `ok:false`, `status:error`, `INTERNAL` with
  `LIVE_FRAME_UNAVAILABLE: the ART session ended before its ready line … outcome=Crash`
  and the child's stderr tail. No Rust-VM fallback and no preview pass occurred
  (`verify.rs` engine selection is unchanged).

## 5. Requests to `crates/acos-art` (W4) — none blocking

The engine needs nothing new from the session protocol to run the loop. Two
optional, exact requests (write them only if useful):

1. **Guest-side frame time.** Add `"produced_at_ms": System.currentTimeMillis()`
   to `frameJson()` in `SessionMain.java` (and `png_bytes` already exists as a
   derived host field). The engine currently stamps the *host receive* time; a
   guest timestamp would let a future check prove frame production happens after
   the tap was delivered, not merely after the reply was read. The engine would
   read it if present and keep the host time as `captured_at_ms`.
2. **Frame-file cap.** `publishFrame()` writes `frame_<seq>.png` for every
   command and never deletes old files; a long session grows `--out` without
   bound. A retention rule (`keep the newest N` or delete after the reply is
   sent) would bound disk. Today the engine reads each file immediately and the
   scenario is short, so this is not a blocker.

No request for `type_text`/`rotate`/`logs` here: they stay honestly unsupported
until the guest has the command.

## 6. Pre-existing failures (not this work)

In this shared tree `cargo test -p acos-cli` also fails:

- `tests/session.rs::launch_counter_reports_the_honest_stage_ladder` and
  `::warm_boot_cache_reaches_first_frame_on_every_launch` — the Rust-VM launch
  stops at `on_resume` with `android.os.BadParcelableException: Parcel data not
  fully consumed, unread size: 796` from
  `IWindow$Stub.onTransact` on `addToDisplayAsUser` (window/parcel workstream).
- `tests/negative_verify.rs::broken_fixtures_never_false_pass` — same launch
  regression, so a broken fixture reports `unsupported` instead of `fail`.

Proof they are not caused by this slice: with these four engine files restored to
their pre-V3 (V2) content and `tests/verify_engine_art.rs` removed,
`launch_counter_reports_the_honest_stage_ladder` still fails with the identical
message (7.46 s). None of the failing paths use `EngineBackend` or `--engine art`.

## 7. Blocker and smallest next action

**Blocker:** on this host the real ART guest crashes before the ready line
(`host-checks/real-art-session-crash-stderr.txt`, `outcome=Crash`, 339 ms, after
`dispatchApplyWindowInsets`), so there is no live frame to capture. The engine
side is ready the moment the session hands over a frame.

**Smallest next action (not in this file set):** fix the crash in
`SessionMain.publishFrame`/window path (W2/W3 in flight), then run

```bash
target/debug/acos verify --scenario scenarios/v2-art-counter.yaml \
  --apk fixtures/build/apps/counter.apk --engine art --json
```

Success is: `status: pass`, `engine: art`, `mode: live`, tap steps at increasing
`frame_seq`, `capture` newer than its tap, and no survivors in the session's
process group. If any step has no live frame the run must fail with
`NO_FRESH_FRAME` / `LIVE_FRAME_UNAVAILABLE` — never a preview or Rust-VM pass.

Host safety: every build/test under `tools/guarded.sh --mem 6G`; the one real ART
session under `--mem 4G --time 170`; peak reported by the launcher 58 MiB cgroup,
no OOM kills; the worktree and its target dir used for the pre-existing-failure
check were removed.
