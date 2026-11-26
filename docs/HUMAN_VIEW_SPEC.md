# Interactive live view — spec (HUMANVIEW1)

Status: **implemented** on branch `humanview` (ADR-0033 amends ADR-0027 §5; CP1–CP5 complete, awaiting live T3 browser E2E & owner Gaten check; 2026-11-25). Owner request: "humans should be
able to interact like a normal emulator". This replaces the T3 Device panel as the
human path. The adb / T3 compatibility work stays parked in PR #5.

## 1. Goal

A human can open a window on any live ACOS session and use it like an emulator
window:
- watch the screen live and smoothly;
- tap, drag, long-press, scroll and type;
- press Back, Home and Recents, and rotate.

Agents keep driving the same session at the same time.

The window is the existing `acos view` page (`docs/LIVE_VIEW.md` §8), made interactive.
It opens in any local browser **and in T3 Code's built-in browser tab**, so it works
inside T3 with no T3-side support and nothing to configure.

## 2. Non-goals

- adb, the T3 Device panel, `agent-device` compatibility. That stays in PR #5
  (`devpanel`), open and parked; see §9.
- A native OS window. It can be added later on top of the same input API if wanted.
- Remote access. The hub stays 127.0.0.1-only and token-gated (ADR-0027 §1).
- Recording, screenshots-to-disk, file drag-and-drop, clipboard sync (later, if asked).

## 3. User flow

1. `acos view` prints `http://127.0.0.1:<port>/?t=<token>` (unchanged), or
   `acos view --open` opens it in the default browser.
2. The page lists live sessions by name. The name comes from `device_name` when the
   agent set one, otherwise `<app label> · <session_id>`.
3. Click a session. The live screen fills the window at the correct aspect ratio,
   with a toolbar: Back, Home, Recents, Rotate, and an "agent activity" strip showing
   the last actions with their source.
4. Interact. Agents see human input in the event feed as `source: panel`.
5. Several sessions can be open side by side, in tabs or a grid.

## 4. Design

### 4.1 Input: browser → hub → session

- **Transport.** One WebSocket per watched session, `/api/input/<session_id>?t=<token>`.
  It is opened only when the user first interacts, so watch-only costs nothing extra.
  Same Host check, token check and connection cap as the existing streams. The
  ADR-0027 §1 rules are unchanged.
- **Messages** (JSON, display coordinates, so the hub does no scaling):
  - `{"t":"touch","phase":"down|move|up|cancel","id":0,"x":540,"y":1200}`.
    Pointer events give real drags and fling velocity, and multi-touch where the
    browser offers it (pinch on a touchpad or touch screen).
  - `{"t":"scroll","x":..,"y":..,"dy":..}` for the mouse wheel, sent as a real scroll gesture.
  - `{"t":"key","code":"KEYCODE_BACK","phase":"down|up"}` for hardware keys and keys
    with no character.
  - `{"t":"text","s":"hello"}` for typed characters, delivered through the focused
    editor's InputConnection, like `acos_text`.
  - `{"t":"rotate","rotation":0|1|2|3}`.
- **Hub → session.** The hub sends each message on the session's existing input path:
  the same `InputDispatch` as the agent tools, with `source: panel`. There is **no new
  input implementation in the guest**. Touch streams use the down/move/up path, so a
  human drag is a real MotionEvent sequence and never collapses into a tap.
- **Ordering.** Messages for one session are applied in order by one worker. A
  disconnect in the middle of a gesture sends `cancel`.
- **Verify runs are watch-only.** For a session owned by `acos verify`, input is
  refused with a visible "watch-only: verify run" banner, and the feed records it.
  This is the rule PR #5 implemented.

### 4.2 Frames

- **Current state.** The hub already streams the producer's PNG/JPEG frames as MJPEG
  and only while someone watches.
- **Changes.**
  - Add `scale: 1.0` and a `max_fps` of up to 30, chosen when a human is interacting.
    Watch-only stays 0.5 scale at 15 fps.
  - Prefer fast adaptive PNG at full size (measured ~10× cheaper to encode than
    scalar JPEG while staying lossless at 1080×2400 @ 30 fps; JPEG remains selectable).
  - Paint immediately on input. The producer is already change-driven, so a frame
    follows within one frame interval.
- **Target.** Input-to-visible-frame **p50 ≤ 100 ms, p95 ≤ 200 ms** on this host
  (measured, §6).

### 4.3 Event feed and agent awareness

Every input event carries `source: agent | panel` (PR #5 §6), so an agent that sees an
unexpected screen can tell a human touched it. The page shows the recent actions from
both sources.

### 4.4 No cost when unused (hard requirement)

- No viewer: unchanged. One atomic load per frame, no threads, no sockets.
- Viewer watching, no input: unchanged from today; the input WebSocket is not open.
- Tracked benchmarks (idle CPU, memory, latency lattice) must match v0.4.1 within gate
  noise, with and without a page open but idle.

## 5. What to reuse from PR #5 (`devpanel`)

Port these as small, focused commits onto a fresh branch from `main`, keeping their tests:
- `source` labels in the input/event feed (`input_dispatch.rs`, `input_channel.rs`) and
  the panel input path;
- verify-run watch-only enforcement (`adb/verify_view.rs` logic, minus adb);
- `device_name` on `acos_launch` (MCP + CLI) and its sanitising;
- live-view fixes: flush pending bytes on an idle screen (CR #4), private 0700 runtime
  dirs (CR #0) where the same code paths are touched.

Do **not** port the adb endpoint, the slot allocator, the shell translator,
`acos-devbridge`, gRPC, tonic/prost, or the RGB888 format.

## 6. Acceptance (definition of done)

**Automated** (in the PR):
1. WebSocket input test, Rust client, live ART session, fixtures:
   - a tap on counter's "Increment" increments it (checked by `observe_tree`);
   - a drag scrolls a list (the visible first item changes);
   - a long press opens a context menu;
   - text typed into an EditText appears in it;
   - BACK finishes the second activity in multi-activity;
   - rotate changes the display size.
2. Input arrives labelled `source: panel`, agent tool input `source: agent`, both
   visible in `/api/events`.
3. A verify run under `acos view` refuses input; its verdict is unchanged.
4. Token, Host check and cap are enforced on `/api/input`: wrong token → 403, wrong
   Host → 403.
5. Gesture fidelity: a drag delivers at least N MOVE events with monotonic
   timestamps, and fling velocity triggers a scroll fling (list moves past the release
   point).
6. Latency: input-to-frame p50/p95, measured by a harness that sends a tap and waits
   for the next frame whose pixels changed, 50 reps. Targets §4.2.
7. Benchmarks: `acos_bench.py --phase memory` plus the latency lattice, against
   v0.4.1, 4 interleaved reps, within noise; idle CPU unchanged with a page open and idle.
8. `tools/verify/check-layout.sh` passes; only `.md` reports are committed.

**Live E2E, done by a subagent with T3's browser preview tools** (`preview_open` the
hub URL in T3's browser tab):
- click the session, click Increment, check the counter changes on screen;
- type into the form app;
- Back and Home work;
- screenshots in the report.

**Owner check (once):** open the page in T3's browser tab and use Gaten by hand.

## 7. Checkpoints (≤ 60 min each, each one committed and pushed)

| CP | Work | Proof |
|---|---|---|
| 1 | Branch from `main`; port source labels, verify watch-only and `device_name` from PR #5 with their tests | ported unit tests pass; `cp3_event_feed_source_labels`-equivalent passes without adb |
| 2 | Hub `/api/input` WebSocket: auth, Host check, cap, per-session ordered worker → InputDispatch (tap/touch stream/key/text/rotate) | acceptance 1 (tap, text, BACK), 2, 4 |
| 3 | Page UI: pointer→touch stream, wheel→scroll, keyboard→key/text, toolbar, session names, verify banner; full-size frames while interacting | acceptance 1 (drag, long press, rotate), 3, 5 |
| 4 | Latency harness + tuning (full-size 1.0x PNG, fps 30 while interacting); benchmarks | acceptance 6, 7 |
| 5 | ADR (amends ADR-0027 §5: `acos view` accepts input, local + token-gated, labelled `panel`); docs (LIVE_VIEW.md §8, agent guide); report `results/handoff/HUMANVIEW1/REPORT.md`; live E2E in T3's browser tab; open PR to `main` (not draft, not merged by agents) | acceptance 8 + E2E screenshots |

## 8. Decision changes

- ADR-0027 §5 ("the viewer is view-only") is amended by a new ADR. `acos view` may
  send input: local only, token-gated, every event labelled `source: panel`, refused
  for verify runs. This is the same rule PR #5's ADR-0033 set for the gRPC path, now
  applied to the page.
- No architecture change: no new process, no new guest service, no new dependency
  beyond a small WebSocket implementation in the hub. Either a tiny hand-written RFC
  6455 framing (the hub is already hand-written HTTP) or `tungstenite`; the
  implementer decides by size, consulting the advisor if unsure.

## 9. PR #5 (adb / T3 Device panel / agent-device) — parked

PR #5 stays open with its status documented in its description:
- devices are listed in T3 with names;
- the panel live view needs serve-emu `inputSource: grpc`;
- `agent-device` tap, type and screenshot work; snapshot, fill, swipe and longpress
  need its helper APK.

The unpushed hub-compat work (worktree `t3-cb31292b`) is committed and pushed to
`devpanel` so nothing is lost. Full stock-T3 compatibility is a later project and
needs an owner decision: a host-side scrcpy protocol with a video encoder, and either
the agent-device helper protocol or app install into live sessions.
