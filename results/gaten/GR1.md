# GR1 — Gaten (RN 0.86.3 / Fabric / Hermes) on ACOS real-ART

Implementer/debugger task. Worktree `/home/Qui/.paseo/worktrees/3oxa6xao/gr1`, branch
`gaten-gr1-rn-art` (off `main` `bf70bb7`). Gaten APK (release, x86_64, unsigned, `dev` `f344272`):
`/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`
(sha256 `e04c7ffc…`, never modified — all runs use it read-only).

**One-line verdict: Gaten boots its own `MainApplication`/`ReactHost`/Hermes/Fabric on real Google
ART, renders its first screen, and answers tree + screenshot + tap — the "RN never reaches ART"
wall in G1 was a *routing* decision in ACOS, not a capability gap in the ART engine.**

## 0. What changed (the wall G1 could not pass)

G1 (§3 ranks 1–2) concluded that RN APKs "never reach real ART" and that reaching Gaten needs a
rewritten RN driver. That was true of the *route*, not of the engine: `crates/acos-mcp`'s RN
backend sends every RN APK to `RnSession` (the frozen Rust VM + a hand-written `BridgeProbe.java`
driver), and `serve` refused `engine: "art"` for anything with an `rn`/`flutter`/`native` flag
(`crates/acos-cli/src/engine/serve.rs`). The ART engine itself already boots an arbitrary APK's own
launcher Activity, installs the APK's 23 x86_64 `.so` files (`launch: native libs installed=23
dir=… guest=/data/app-lib/x86_64`), and composites real HWUI surfaces — which is exactly what Gaten
needs.

Fix (commit 1): `acos launch --engine art` now routes an RN APK to the ART launcher:

* `crates/acos-cli/src/launch.rs` — new `--engine {art,rust-vm}` flag. With `art` the RN/Flutter
  *product-path* detection is not applied (`rn`/`flutter` stay false), the Rust loader's `--native`
  is refused with a structured `INVALID_ARGUMENT`, and the launch goes through `acos serve` (the
  ART guest is its own process) carrying `"engine": "art"` verbatim.
* `crates/acos-cli/src/engine/serve.rs` — `engine: "art"` is authoritative: the `rn`/`flutter`
  hints no longer refuse the launch (they are legacy-route selectors, and ART runs the APK's own
  Activity), while `native: true` (the Rust loader) still refuses with a structured
  `UNSUPPORTED_BACKEND`. An unknown engine value is still `UNSUPPORTED_BACKEND`/`INVALID_ARGUMENT`,
  never a fallback.
* Non-RN routing is unchanged: without `--engine`, an RN APK still takes the native-child/RN route
  and a plain app still takes the daemon default.

Result (`results/gaten/evidence/GR1/launch-art.json`):

```
acos launch gaten-dev-f344272-…-unsigned.apk --engine art --json
status pass  engine "art"  engine_frozen false  launched true
stage_reached "first_frame"  stages [process ok, activity_resumed ok, first_frame ok]
total_ms 764
```

The honest-error side of G1 rank 5 is satisfied by construction: `--engine art` never reaches
`rn_backend`/`RnSession`, so no reply can be labelled `rust-vm, frozen`; a failure on the ART path
is an `art_error`/`BackendError` from the ART session (structured `GUEST_EXITED`/`STAGE_BLOCKED`).

## 1. Walls hit, in order (hypothesis ledger)

| # | observation | hypothesis | experiment | result |
|---|---|---|---|---|
| W1 | G1: RN APK + `serve --engine art` → `UNSUPPORTED_RN_DRIVER`, engine label `rust-vm, frozen` | routing, not engine capability | read `serve.rs::call_launch` + `session_backend.rs::call_run` | **confirmed**: `rn: true` forces the legacy route and `engine: art` was refused outright |
| W2 | after routing fix, does the APK's native closure load under ART? | the ART guest uses stock bionic linker + `PathClassLoader` native library dirs | `acos-art launch` stderr | **confirmed**: `launch: native libs installed=23 dir=… guest=/data/app-lib/x86_64`; `libhermesvm`, `libfbjni`, `libappmodules`, `libonnxruntime` all load |
| W3 | first screen | RN 0.86 Fabric mounts real `android.view.View`s, so the normal guest view probe sees them | `tree` on the live guest | **confirmed**: 100+ node tree, `com.gaten:id/action_bar_root`, `FitWindowsLinearLayout`, `SvgView` nodes, text `Set up Gaten`, Japanese text `なるほど、合点がいった。` |
| W4 | first frame | C1 buffer captures work for a native-closure app | `screenshot` | **confirmed**: 1080×2209 PNG, `capture_source: acos-c1-buffer`, `buffer_generation 83` (rendering continuously) |
| W5 | `tap "Set up Gaten"` → `NO_FRESH_FRAME` after 21.5 s, but the next screenshot shows the transition began | the tap landed; the settle detector never sees a "quiet" frame because the transition is a long continuous animation (Reanimated spring + artwork drift) | tap, then screenshot at +2 s | **confirmed the tap works** (`02-after-setup.png` shows the welcome screen animating out, a large glyph and circle entering). The 21.5 s is a *settle* budget, not a stuck app — see §4 |
| W6 | onboarding tree is offset: the pager container's `bounds_on_screen` is `[270,128][1350,2337]` while the window is `[0,128][1080,2337]`; welcome artwork is at `[58,…][1022,…]` | the onboarding pager is translated by +270 px (off-screen pages), so it is not the visible layer; the *real* defect is the wordmark drawing, §5 | bounds of every node vs the captured PNG | **open** — see §5 (visual verification) |
| W7 | guest teardown after `close` does not finish within 120 s on this app (host-side `acos-art` process stays) | GL1's separate teardown/2-minute-shutdown bug | observed twice | **not mine** (owned by GL1); noted, not touched |

## 2. Evidence

`results/gaten/evidence/GR1/`:

| file | what it is |
|---|---|
| `launch-art.json` | `acos launch --engine art` envelope: `first_frame`, `engine: "art"`, 764 ms |
| `gaten-art-00-welcome.png` | very first captured frame (frame 1, 1080×2209 → 405×828 here) |
| `gaten-art-01-welcome-settled.png` | same screen 2 s later (buffers still advancing) |
| `gaten-art-02-after-setup.png` | 2 s after `tap "Set up Gaten"`: the welcome screen is animating out |
| `tree-first-screen.json` | the live view tree of the first screen (trimmed to text/desc + bounds) |

Timings (host wall clock, `run-exclusive.sh` + `guarded.sh --mem 8G`): `acos-art launch` ready line
**714 ms** after spawn; `acos verify --engine art` launch step **1559 ms**; `acos launch --engine art`
through the daemon **764 ms** total; first `screenshot` ~25 ms; `tree` ~150 ms.

## 3. What still fails / next wall

* **Settle after a tap on an animated screen**: `tap` returns `NO_FRESH_FRAME` although the tap
  landed (`state_changed` is not reported and the frame hash does change afterwards). Next: use a
  shorter `ACOSLAUNCH_FRAME_WAIT_SECS`/`ACOS_ART_SETTLE_MS` and assert on the *post-tap* frame, or
  document that an animated transition needs `capture: fresh` rather than the tap's own frame.
* **Onboarding → home**: the app's real first screen at `f344272` is the *Welcome* screen
  (Gaten wordmark, "See a word you don't know", "Set up Gaten"); the home screen described in G1 §2
  is behind three Skip pages (`OnboardingFlow.tsx`). Navigation through the flow is the next step.
* **Teardown** (W7) is GL1's.

## 4. Visual verification vs the owner's Moto G35
(filled in below once the reference captures exist)
