# W7B handoff — `report` inset default, w7 gate red, w42 re-bless

Branch `fix-w7b-insets` (off `main` 3e11d9c). Worktree
`/home/Qui/.paseo/worktrees/3oxa6xao/g3cd`; target `.cache/g3cd-target`.
All ART runs under `tools/art-runtime/run-exclusive.sh --task W7B` +
`tools/guarded.sh`; scratch under `.cache/w7b/`.

## 1. Goal A — why `w7-insets-report` was red on main

**Root cause: the gate ran the probe under the ambient `carve` default, not
because of the density fix.** `scenarios/w7-insets-report.yaml` documents "run
with `ACOS_WM_INSETS=report`" but no gate runner sets it, and W7 left `carve` as
the compiled default (`acos-wm/src/display.rs::DEFAULT_INSETS_MODE`). Under
`carve` the WMS hands `[0,128][1080,2337]`; `InsetsState.calculateInsets(mWinFrame, …)`
finds no status/nav source inside that frame and the probe prints `status=0`
(gate: `steps/03-assert_text.json`, tree `decor [0,128][1080,2337]`). With the
model actually selected the same binary prints `status=128`, `nav=63`,
`decor=[0,0][1080,2400]`.

**Bisect / A-B (targeted, same binary):**

| arm | flags | model | probe |
|---|---|---|---|
| main, env `report` | manifest flags `0x10883e46` | report | **pass** (`.cache/w7b/out/report-main.json`) |
| main, env `report` + `ACOSLAUNCH_APP_FLAGS=0x10808044` (the pre-0e569b5 flag set, emulating 5cae694 at the exact seam the density fix changes) | old flags, no `FLAG_SUPPORTS_*`/density bits | report | **pass** (`.cache/w7b/out/report-oldflags.json`) |
| main, default | manifest flags | carve | fail at `status=128`, got `status=0` (`.cache/w7b/gates/w7-insets-report.json`) |

The other deltas in `5cae694..main` are bench-only: B4 `78037b9` touches
`tools/bench*`, `results/**`, `research/**`; S3c `b6bec51` touches
`tools/benchlib.py` and bench harnesses. Neither touches the launcher, WMS,
guest or verifier, so they cannot move an insets read. **The density hunk is
not the regression, and it stays.**

AOSP cross-check (tag android-14.0.0_r75, `.cache/aosp/base/core/java`):

* `PackageParser.parseBaseApplication` (`PackageParser.java:2500-2505`) sets
  `FLAG_SUPPORTS_SCREEN_DENSITIES` for target >= DONUT when `<supports-screens
  android:anyDensity>` is absent; the density fix transcribes this.
* `CompatibilityInfo` (`CompatibilityInfo.java:295-310`) turns the *absence* of
  that flag into `SCALING_REQUIRED` + `applicationScale = DENSITY_DEVICE/160`
  (2.625 here). It is the only framework consumer of the bit that touches layout.
* `ActivityThread.handleBindApplication` (`ActivityThread.java:7218-7225`) also
  uses it only for `mDensityCompatMode` / `Bitmap.setDefaultDensity(160)`.
* `ViewRootImpl.setView` (`ViewRootImpl.java:1407-1438`) only installs a
  `CompatModeTranslator` from the process `CompatibilityInfo`;
  `handleResized` (`ViewRootImpl.java:2098-2106`) would then translate the
  `InsetsState` from screen to app coordinates. ACOS's guest always binds
  `CompatibilityInfo.DEFAULT_COMPATIBILITY_INFO`
  (`LaunchMain.buildBindApplicationRequest`), so `mTranslator == null` on every
  run and the flag/scale path cannot change the insets the probe reads.
* The zero comes from geometry, not density: `WindowLayout.computeFrames` /
  `PhoneWindow.generateLayout` (fitInsetsTypes 0 for a non-floating Activity)
  and `InsetsState.calculateInsets(windowFrame, …)` intersect the source frames
  with the window frame. A carved frame excludes the bars, so the calculation is
  zero — exactly the ACOS W7 carve model.

The gate was therefore red by construction: the positive scenario needed the
env/default it never pinned. Fixing the default (Goal B) plus pinning the model
in both W7 scenarios (below) makes the gate honest; no assertion was weakened.

## 2. Goal B — `report` is the default

* `acos-wm::display::DEFAULT_INSETS_MODE = InsetsMode::Report` and
  `InsetsMode::default() = Report` (ADR-0026 decision 6). `carve` stays
  selectable via `ACOS_WM_INSETS=carve`, `acos launch/verify --insets carve`,
  `acos-art launch --insets carve`, and a scenario's `insets: carve`.
* New per-scenario field `insets: carve|report` (`acos-protocol::scenario`,
  validated) threaded through `acos verify` with precedence
  **CLI `--insets` > scenario `insets` > `ACOS_WM_INSETS` > build default**.
  `acos verify` installs the resolved value in the process env before the
  session starts, so guest, WMS shim, DisplayInfo and verifier frame metadata
  use one model. The verdict publishes it (`insets`), so a pass cannot be
  attributed to the wrong model.
* `scenarios/w7-insets-report.yaml`: `insets: report`.
  `scenarios/w7-insets-carve-negative.yaml`: `insets: carve`, and it now first
  asserts the carved frame `[0,128][1080,2337]` (proves the selector switched
  the model) then fails the report contract at `status=128` (`status=0`). After
  the default flip this is essential: without the pin, the negative would pass
  and be a false pass.
* `docs/AGENT_API.md` §8.6.6 (default + surfaces + scenario pinning),
  `docs/SECURITY.md` env table, `docs/MORNING-2026-11-13.md` W7 row,
  `docs/TRACKS-2026-11-12.md`, `results/render/W7.md` addendum.

## 3. w42 re-bless — one known anti-aliased row

`w42-back-frame` is pinned to `insets: report` (its re-blessed hash is a
display-sized, origin-[0,0] frame). Only the **final first-Activity region 1**
hash changed:

```
dee05db89a0d3856d6bb88f91fa271ef36f7429c496a3a48dc69c2ffb7d0746d  (carve)
5edac422cd1d5530ca12b5d4b5285a8e29a938861a126557bd84370206eb86df  (report)
```

Pixel diff report vs carve, frame 14, overlapping display rows 128..2336
(`results/handoff/W7B-pixel-diff.txt`, artifacts
`.cache/w7b/art-w42-report/frames/14.png` and
`.cache/w7b/art-w42-carve/frames/14.png`):

* report PNG `1080x2400` origin `[0,0]`; carve PNG `1080x2209` origin `[0,128]`
* **exactly one** differing row: display y=411 (carve PNG y=283), 122 pixels,
  x=58..1026, channel delta only `G -1` (carve G 211 -> report G 210)
* all other pixels in the overlap identical; second-activity region 1/2 and the
  final region 2 assertions are byte-identical across the models (they pass in
  the report run). No tolerance was added.

## 4. Verification (final release binary)

Gates (`.cache/w7b/run_gates.sh`, results in `.cache/w7b/gates/`):

```
v2-art-counter                    pass   insets=report
w22-counter-reset                 pass   insets=report
w54-dialog-pixels                 pass   insets=report
w42-back-frame                    pass   insets=report
w15-dialog-menu                   pass   insets=report
form-validation                   pass   insets=report
suite/T03-render-good             pass   insets=report
v1-offscreen-pager                pass   insets=report
w7-insets-report                  pass   insets=report
w7-insets-carve-negative          fail ASSERTION_FAILED steps=4 insets=carve
suite/T03-render-wrong            fail ASSERTION_FAILED
negative/offscreen-pager-visible  fail ELEMENT_OFFSCREEN
p11-progress-timeout              timeout TIMEOUT
```

Gaten (`.cache/w7b/run_gaten.sh`, `.cache/w7b/gaten/`): gaten-home/tabs/settings/
search/lesson **pass** (14/22/33/22/28 steps, `insets=report`); all five
`scenarios/negative/gaten-*-wrong.yaml` fail `ASSERTION_FAILED`.

Tests: `cargo test --release --offline -p acos-apk -p acos-cli -p acos-verify
-p acos-services` green (acos-services' carve wire-golden tests now pin
`ServiceHost::standard_with_insets(InsetsMode::Carve)` instead of relying on the
process env; `DisplayInfo` gets the same explicit mode). `acos-art`:
`-- --test-threads=1 --skip refuses_to_run_when_no_cgroup_can_be_created` green,
then the skipped test alone in a **fresh** lock acquisition (fresh
`ACOS_ART_DATA`, hence fresh capability cache) green — running it in the same
acquisition as the skip arm reuses a cached `scope_ok=true` and fails, which is
the known order dependence.

Scenarios that depended on carve: only the two W7 probes mentioned it; the
w42 hash was model-specific and is now pinned. All other gates are byte-
identical under `report` (W7 §4 and this run).

## 5. Not fixed here

* **GR4 Backlog G4a** (visibility vs carved bottom bar): with `report` the
  Activity window is the full display, so the carve-specific clip that G4a
  describes (the carved local visible rect cutting the bottom row) no longer
  applies to Activities; dialogs keep their `systemBars()` fit. The default flip
  therefore likely removes G4a's trigger, but GR4 must re-measure it. Not fixed
  or re-scoped in this track.
* D55/other baselines that genuinely want the old pixels must pin
  `insets: carve` explicitly.

## 6. Incident (external, not caused by this track)

At ~14:06 local an external cleanup removed `.cache/` from this worktree (and
`k5`, `gr4`, `s3ab`; ~92 GB freed host-wide), including the required symlinks
and the prebuilt `g3cd-target`. The symlinks (`fixtures/build`, `.cache/art`,
`.cache/boot/android-34`, `.cache/gaten`) were recreated and the release build
redone (2 m 17 s). Other tracks' scratch evidence under `.cache` is gone.
