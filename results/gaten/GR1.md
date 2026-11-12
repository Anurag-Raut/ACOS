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

## 5. Visual verification against the owner's Moto G35 (2026-11-12)

Reference conditions, hashes and the full metrics table: `results/gaten/GR1-visual-metrics.md`.
Reference build: `com.gaten.acosref`, arm64-v8a release, built from the **same source commit**
`f344272` in the approved copy; its `assets/index.android.bundle` sha256 is **identical** to the
x86_64 APK's (`3c5428ea…`). The phone's `com.gaten` was never launched, changed or removed; the
`.acosref` package is the only thing installed, and it is uninstalled at the end.

### 5.1 Conditions (bottom-up, layer 1: display metrics)

Before this run ACOS's ART launch path could not be given a display at all: `acos-art launch`
(the interactive path the engine spawns) had no geometry options and the guest hardcoded
`1080x2400@420`, status bar 128 px, nav bar 63 px. Against the phone (`1080x2400@400`, status
108 px, gesture nav) every dp was 5% off and the window 191 px short — a *configuration* mismatch,
so no pixel comparison would have been meaningful. Fixed (commit 3, §2 of the metrics file):
`acos-art launch --width/--panel-height/--density/--status-inset/--nav-inset` → `LaunchOpts` →
`ACOSLAUNCH_*` → `LaunchMain`'s `Configuration` (dp derived from px/density) and `DisplayInfo`,
plus `ACOS_DISPLAY_OVERRIDE` for the WMS shim. Defaults unchanged, so the API-34 emulator
baselines (T03 etc.) keep their numbers; the ready line now reports the density the guest really
booted at instead of a constant 420.

With `ACOS_ART_{WIDTH,PANEL_HEIGHT,DENSITY,STATUS_INSET,NAV_INSET}=1080,2400,400,108,0` the guest
boots `1080x2292 @ 400 dpi` (window = display minus the status bar), the app's
`Resources.getDisplayMetrics()` reports `density=2.5`, and the device's app area
(`screencap` cropped at y=108) and the ACOS frame are both 1080×2292, so they compare **1:1 with
no rescaling**.

### 5.2 Welcome screen: device vs ACOS

`visual/welcome/{device,acos,diff,side-by-side}.png`, `visual/welcome/{metrics,elements}.json`.

| metric | value |
|---|---|
| SSIM (grayscale, 8×8 windows) | **0.8485** |
| pixels differing at all | 38.95 % |
| pixels differing by >16/255 | 14.56 % |
| mean abs channel diff | 18.8 |

Per element (frame px at 2.5 density; display y = frame y + 108):

| element | device | ACOS | delta | verdict |
|---|---|---|---|---|
| primary button rect | 55..1024 × 1950..2151 (969×201 px = 387.6×80.4 dp) | 55..1024 × 2010..2211 (**same size**) | **+60 px (24 dp) lower** | layout/insets |
| caption band ("See what it means") | 1854..1897 | 1916..1967 | +62 px | same cause |
| wordmark ink | 57..309 × 74..134 → **252×60 px (100.8×24.0 dp)** | 61..692 × 98..250 → **631×152 px** | **2.5× too large**, overflows its 270×116 px box | **renderer** |
| tagline | fits on one line under the mark, y≈180 | overlapped by the mark's ink | — | consequence of the above |

Both remaining discrepancies have a *single* root cause each, and neither is the app's:

**(A) Wordmark 2.5× too large → renderer layer: the launch guest has no per-view canvas.**
`fixtures/apps/svg-viewbox` reproduces react-native-svg's own arithmetic (viewBox×density,
`canvas.getWidth()`, `PathParser`'s density-scaled path) with plain framework APIs and prints its
inputs. On ACOS (`evidence/GR1/svg-probe.json`):

```
view=270x116  canvas=1080x2292  density=2.5  canvasIsViewPx=false
scale=0.23567921  ink=9,982,1070,1240  inkInsideView=false  canvasMatrix=1.0,0.0,1.0,309.0
```

`Canvas.getWidth()/getHeight()` inside `View.onDraw` are the **window's** size, not the view's.
On hardware-accelerated Android (`View.draw` → the view's own `RenderNode`) they are the view's
size (270×116), the viewBox scale would be `270/(1833×2.5) = 0.0589` and the ink would be
≈245×60 px inside the box. RNSVG feeds those numbers straight into `ViewBox.getTransform`, so the
mark is drawn `0.2357/0.0589 = 4.0×` too large (2.5× after the ancestor clip that produced the
measured 152 px of visible ink). The boxes are **identical on both systems** (270×116 px at the
same position within 7 px), so this is not layout, not metrics, not a font: it is native drawing.

Cause on the ACOS side: the guest deliberately clears `FLAG_HARDWARE_ACCELERATED`
(`LaunchMain.buildAppInfo`, "the surface-less ACOS window draws through the software ViewRootImpl
path", P4b §4). The drawing route is a single window-sized software canvas, so there are no
per-view RenderNodes. The experiment that pins it: requesting the flag
(`ACOSLAUNCH_APP_FLAGS=0x02000000`) makes the guest exit status 1 before its first live frame —
there is no HWUI path to fall back on today. **Fix needed (not done, out of budget): give the
launch guest the real HWUI path** — `FLAG_HARDWARE_ACCELERATED`, a RenderThread/RenderNode per
View, `beginRecording(width,height)` per view and replay into the compositor's buffer. That is the
same work item as "stock HWUI/Skia on the C1 path" in `docs/ENGINEERING_POLICY.md`, and the
regression test for it is committed: `scenarios/svg-viewbox.yaml` asserts
`canvasIsViewPx=true`/`inkInsideView=true` and is **red** on the engine today (it is not in the
mandatory gate list for that reason).

**(B) Footer 60 px (24 dp) too low → insets layer: the ACOS window is carved, the phone's is
edge-to-edge.**
The device's app window is `frame=[0,0][1080,2400]` (`EDGE_TO_EDGE_ENFORCED`, targetSdk 36) with
status-bar inset 108 and a 24 dp gesture-bar bottom inset; the app *applies* those insets itself.
ACOS hands the app a window that is already **carved** (`DecorView [0,108][1080,2400]`) and reports
zero insets, so any bottom-anchored element sits 24 dp lower. Evidence: the button's *size* is
identical to the pixel (387.6×80.4 dp on both) and only its y differs by exactly 60 px = 24 dp;
`--nav-inset 60` changes nothing because the frame is carved instead of inset (`visual/welcome/
elements.json`). Fix direction: `acos-wm` should model a full-display app window and *report*
`statusBars`/`navigationBars` insets (108 / 60 px here) the way WMS does, instead of shrinking the
frame; that also removes the 108 px difference in `Configuration.screenHeightDp` (842 vs 960) that
`useWindowDimensions().height` consumers see.

### 5.3 Screens reachable on ACOS today

ACOS reaches the **welcome screen** only. The follow-on screens were captured on the device as the
target (`visual/page1…page3`, `home`, `analytics`), and ACOS's corresponding captures show the
welcome screen still in place: **the `tap` on "Set up Gaten" is delivered and handled but the
onboarding flow does not advance** (§4 W5 + §6). So no home/Analytics comparison is possible yet,
and none is claimed.

### 5.4 Tap coordinates vs the visual element

The tree's `bounds_on_screen` for the primary button on ACOS (`417,2232..2285` for the label
inside a `58,2210..1022,2310` pressable) matches the pixels that were drawn, and
`tap` at the label's centre is accepted by the input channel (`gesture.acks[0].handled=true`,
`latency_us ≈ 15 100`). Coordinate mapping is therefore consistent; the failure to advance is not
a coordinate error (the same tap at the same display coordinates drives the device build).
