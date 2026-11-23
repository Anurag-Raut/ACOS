# ACOS: state and plan, 2026-11-15 (pick up here)

Written at the owner's request so work can resume cold. Facts below were checked
against the repo and the session on 2026-11-15; items marked *inferred* were not
tested.

## 1. Rules the owner set (read first)

- **Root cause, complete features.** Group feedback by shared root cause and fix
  each once, properly. Do not bundle "quick wins" into a release. Shipping nothing
  in a cycle is fine. (Memory: `acos-root-cause-complete-features`.)
- Verification honesty is unchanged: 0 false passes, never weaken a test or gate,
  an unsupported thing is a structured `unsupported`, never a silent stub.
- Orchestration: Opus plans and judges; Claude implementers do the work because
  OpenCode Go is exhausted until about 2026-11-16 05:00 IST. Sonnet 5.5 medium for
  difficult work (verdict semantics, security, framework internals), Haiku 5.5
  effort high for easier work. Back to pi + DeepSeek after the reset. Max 5 agents.
- ART runs go through `tools/art-runtime/run-exclusive.sh` + `tools/guard/guarded.sh`;
  RAM < 90%; prune scratch at disk >= 85%; never touch `~/Projects/gaten`, foreign
  `acos serve` / `acos-mcp` processes or the owner's phone data. I install
  releases into `~/.local` myself; agents never do.

## 2. What was done

**Released: v0.2.2** (origin main `546e7b38`, tag `v0.2.2`, installed in
`~/.local`, MCP registered and connected). Notes: `results/release/v0.2.2-NOTES.md`.
Verified by: build, all 15 gates (positives pass, negatives fail as intended),
Gaten 5+5 scenarios, and counter / gaten / gaten-search MCP smokes from a fresh
install. Deliberately light: the long ART crate tests, GR4 and an A/B were skipped.

Contents: live view (`acos view`), action replies with `changes` (FB3), live
typing over MCP (FB1), `observe_tree {"screenshot": true}` and placeholder
evidence (FB2c), idle-session hygiene (FB5), agent docs generated from tool
descriptors (FB6), APK opened once per launch (PERF-REL3b/c, Gaten manifest step
about 63 ms vs 378 ms), plus these found-and-fixed bugs: fresh installs could not
launch any app (never shipped), the default `acos_help` was over its 3 KiB
budget, and `acos-mcp` returned truncated output as success when a child exited
fast.

**Live view specifics.** `acos view` prints one 127.0.0.1 URL with a token. Only
the phone screen is shown, scaled to fit. In T3 Code open the URL in the preview
pane and pick a portrait phone preset (Pixel 7); no T3 patch needed. Security
re-check by a fresh Opus instance: MERGE (socket hidden from the guest, masking
of password text and keys, FLAG_SECURE frames black in the stream only, Host
check). A 22/22 live proof script lives at
`results/handoff/VIEW1c-evidence/drive.py`.

**Dogfood.** A fresh Haiku agent used v0.2.1 on Gaten through MCP only and scored
it 4/10 (`.cache/rel2b/FEEDBACK.md`; newer entries in
`~/.claude/acos-feedback/ACOS_AGENT_FEEDBACK.md`). Typing, screenshots and
action replies from that list are already fixed in v0.2.2.

## 3. Unshipped local work (decision: do not ship now)

Branch `int-022`, worktree `.cache/wt-int`, two commits on top of main, not pushed,
not tagged, not installed:

- `3fcc5db4` live view: a viewer that attaches to an idle screen is sent the
  current frame. Root cause: the encoder only sends a frame whose `seq` is newer
  than the last one it sent, so a viewer opening an unchanged screen saw black.
  Fix resets the last-encoded seq on attach. Regression test
  `a_viewer_that_arrives_on_an_idle_screen_still_gets_the_current_frame` fails
  without the fix; live check: every visit to an idle Gaten session got a frame.
- `a54a7cef` release: 0.2.3 bump + `results/release/v0.2.3-NOTES.md`.

The owner said not to ship this as its own patch. Keep both commits; fold them
into the next themed release (they are a real root-cause fix, not a patch).
Not yet run on it: the gates, ART crate tests, A/B.

Other known viewer gaps (not fixed): Enter in a password field can stream the key
code (never the text); demo sessions older than about 6 minutes stopped
producing frames while taps still ran (taps reported `pixels_changed: false`,
settle `probe-floor`), which needs investigation.

## 4. Unmerged branches (from before this session's release)

- `art-back-1` (`.cache/wt-rot7`, `eb74880e`): ROT8b, the Back-navigation fixes
  (`task_finished` false-pass, `onSaveInstanceState` negative, stale PNG rebind).
  Two files have uncommitted edits (`atms.rs`, `rot8b_task_finished.rs`). ROT9
  (layer pool) not started. Needs its own full verification before merge.
- Merged and removable: `fb2-shot-placeholder`, `fb3-act-diff`, `live-view-2`,
  `perf-rel3b`, `int-022-fb3`. Worktrees `.cache/wt-mfb3`, `.cache/wt-perf`,
  `.cache/wt-view1` and the two outside `.cache` can be removed once the owner
  agrees (archive dirty work first).

## 5. The plan: four root-cause tracks (each ships as its own finished release)

Recommended order is 3, 2, 1, 4 because 3 hurt most in the dogfood runs.

1. **Docs drift** (stale RN "no tree, no frames" text, coordinate-origin
   confusion, missing limits, auto-asserted taps). Root cause: hand-written text
   that contradicts code. Fix: generate limits and capabilities text from the same
   table `acos_capabilities` reports, and add a test that fails when docs and
   code disagree.
2. **Reply size budget.** Launch 67-111 KB, observe cuts out the middle of the
   tree, verify tree artifacts keep 1 node. Root cause: every reply inlines
   everything. Fix: one reply rule for all tools: hard cap, compact default that
   answers "what is on screen" (texts + bounds), file path for the rest; same rule
   inside verify artifacts. Decide compatibility for the launch tree default first.
3. **Settle semantics.** `wait_for` times out on always-animating RN screens,
   `assert_text` ignores `timeout_ms`, a tap can return `pass` with
   `settled: false`. Root cause: settle is defined by pixels. Fix: content
   stability as a first-class settle mode used by every wait, assertion and
   action reply; replies state which mode was used; a timed-out settle is never a
   plain pass. This touches verdict rules: Sonnet implements, a fresh Opus-medium
   reviews it.
4. **Handled vs sent.** `state_changed: false` cannot distinguish "nothing was
   tappable" from "the app ignored it". Fix: the input dispatcher reports whether
   a handler consumed the event; replies carry it.

## 6. The plan: service modules (agents request and attach services per session)

Gap list (code-checked unless marked *inferred*):

- **Not registered at all** (framework gets null and takes its missing path):
  wifi, telephony/phone, bluetooth, nfc, usb, camera, vibrator, midi, biometric /
  fingerprint, device policy, lock settings, role, media session / router,
  wallpaper, print, search, launcher apps, usage stats, battery stats, dropbox,
  carrier config, country detector, webview update.
- **Fixed "offline device" answers only:** accessibility (reports disabled),
  audio (silent), connectivity (default-network state machine; real sockets
  blocked, `set_network` scripts responses; real network opt-in is queued as
  METRO and needs a security review), alarm and jobscheduler (accepted, never
  fire), power, thermal, account (empty), storage (one emulated volume), location
  (no geocoder), trust/keyguard, notification (state, no shade), shortcut (no
  launcher), autofill, content capture, voice interaction, game, virtual device.
- **System model:** one package, one user, one app process; services or receivers
  declared for another process are not run separately (comment in `launch.rs`,
  *untested*); intent resolution sees only the app under test, so share sheets,
  pickers, browser and camera app go nowhere; no WebView (package manager reports
  the feature absent); no rotation, fold or multi-window.
- **Absent system apps/UI** (*inferred*): lock screen, status bar and shade,
  Settings, launcher, permission-grant dialogs, system content providers
  (contacts, calendar, media store), Google Play services, real GPS / camera /
  sensors / biometrics / SMS and calls.

Target design (not yet an ADR; needs owner approval):

- Each service is a module with a **profile**: `off`, `stub`, `scripted`, `real`.
- A session asks for services at launch (`services: [...]`); the launch reply and
  `acos_capabilities` report exactly what was attached.
- An unattached service returns a structured `unsupported`, never a silent null.
- Every module has a RAM and startup budget, a seccomp / permission review, and
  a test that proves it reports honestly. This extends ADR-0008 ("start only what
  a task needs").

Build order (owner: all of these are wanted; order is my recommendation):

1. Network, real or scripted.
2. Second app and intent routing (share sheets, pickers, browser, camera app).
3. Notifications and permission dialogs.
4. WebView.
5. Accessibility service and overlay windows (the Gaten floating bubble needs
   this; today `GatenAccessibilityService` never starts).
6. Camera, location and sensor feeds (scripted input).
7. Telephony, Bluetooth, biometrics.

First step when resuming this track is a design note per module (interface,
attach API, cost budget), then owner approval, then code. Nothing for this track is
built.

## 7. Backlog parked for later

GUESTEXIT0 flake (gr4-recognition guest exit 0 before the first frame under
load), ZPROD8, METRO1..8, prune stale `/run/user/1000/acos-view-*/view.sock`,
ABCHECK, Flutter (deferred by owner), moving implementers back to DeepSeek after
the 2026-11-16 reset.

## 8. Running right now

An idle Gaten demo session plus `acos view --port 38417` (started by me, from the
unshipped `int-022` build) are still up; the demo stops by itself and the viewer
can be killed with `pkill -f 'release/acos view'`. Host at last check: disk 83%,
RAM fine, no agents running.

## 9. First actions on return

1. Ask the owner which root-cause track to start (recommended: track 3).
2. Or approve the service-modules design notes and start with network.
3. Decide whether to fold the two local viewer commits into the next release.
