# GR1 visual verification — reference conditions

Reference device: **moto g35 5G, serial ZA222WGJPP** (owner-authorised for this task only), Android 15
(API 35), arm64-v8a. Reference build: Gaten `dev` `f344272` rebuilt in the approved copy
`/home/Qui/Projects/acos/.cache/gaten/src-dev` with `applicationIdSuffix ".acosref"`
(`com.gaten.acosref`, release/R8, arm64-v8a, signed with a throwaway key), so the owner's
`com.gaten` (with real data) is untouched.

Same-inputs proof:

| fact | device | ACOS |
|---|---|---|
| source commit | `f344272` | `f344272` |
| `assets/index.android.bundle` sha256 | `3c5428ea05ac2e9a90ccfef42a3c47e850656fd4744b0febaadde747d6d2300b` | **identical** |
| release build type | R8 full mode, shrinkResources | identical |
| ABI | arm64-v8a | x86_64 (same JS/assets; only the ELF payload differs) |
| onboarding state | fresh install (never launched before) | fresh guest data dir per run |

## 1. Display metrics (read from both systems)

Device values are read-only (`wm size/density`, `dumpsys display/window`, `settings get`); nothing
was changed on the phone.

| metric | Moto G35 (device) | ACOS ART guest (default profile) | ACOS with reference geometry |
|---|---|---|---|
| physical size px | 1080x2400 | 1080x2400 | 1080x2400 |
| app/window size px | **1080x2400** (edge-to-edge, `frame=[0,0][1080,2400]`) | 1080x2209 (`DecorView [0,128][1080,2337]`) | see §2 |
| densityDpi | 400 | 420 | 400 |
| density | 2.5 | 2.625 | 2.5 |
| fontScale | 1.0 | 1.0 | 1.0 |
| xdpi / ydpi | 391.885 / 390.769 | n/a (shim DisplayInfo has one dpi) | n/a |
| status-bar inset px | **108** (== display cutout top) | 128 | 108 |
| navigation-bar inset px | **0** (gesture nav; mandatory-gesture bottom 80) | 63 | 0 |
| `Configuration` | `sw432dp w432dp h960dp 400dpi`, `en_US,ja_JP,en_IN`, light | `sw411dp w411dp h842dp 420dpi`, `en-US`, light | `sw432dp w432dp h960dp 400dpi` |
| `DisplayInfo.appWidth/appHeight` | 1080 / 2400 | 1080 / 2209 | 1080 / 2292 |
| night mode | light (`ui_night_mode=1`) | light | light |
| locale list | `en_US,ja_JP,en_IN` | `en-US` | `en-US` |
| refresh | 60/90/120 modes, active 60 | vsync shim 60 | vsync shim 60 |

Consequences for a pixel comparison: before ACOS is given the reference geometry, every dp-based
size is 420/400 = **5% larger** on ACOS and the window is 191 px shorter (128+63 vs 108+0), so a
difference is expected by construction — that is a *configuration* difference, not an app or
renderer defect. `DisplayOverride::phone()` already encodes the phone's exact numbers
(`1080x2400@400/108/0`), so the fix is plumbing, not a new concept (§2).

## 2. The reference-geometry plumbing (ACOS fix)

`crates/acos-art launch` (the interactive path the ART engine spawns) had **no display options**:
`LaunchOpts` carried only apk/until/out/timeout, and the guest hardcoded
`1080x2400@420` with a 128 px status bar and 63 px navigation bar in
`LaunchMain.displayConfiguration()` and in the `DisplayInfo` reply. `---density/--display/--reference`
on `acos launch` therefore could not reach an ART session at all.

Fix (commit): `acos-art launch --width/--panel-height/--density/--status-inset/--nav-inset` are now
`LaunchOpts` fields, forwarded to the guest as `ACOSLAUNCH_*`; `LaunchMain` derives
`Configuration.screenWidthDp/HeightDp/smallestScreenWidthDp` (dp = px*160/dpi) and the `DisplayInfo`
`appWidth/appHeight` from them instead of hardcoding; the engine forwards the same five values and
sets `ACOS_DISPLAY_OVERRIDE` (the spec `acos-services::display` already parses) so the WMS shim's
window frame and the insets the app sees come from one source. Defaults are unchanged, so the
existing API-34-emulator baselines (T03 etc.) keep their exact numbers.

Reference run: `ACOS_ART_WIDTH=1080 ACOS_ART_PANEL_HEIGHT=2400 ACOS_ART_DENSITY=400
ACOS_ART_STATUS_INSET=108 ACOS_ART_NAV_INSET=0`.

## 3. Per-screen comparison

See `results/gaten/evidence/GR1/visual/` (`<screen>/{device,acos,diff,side-by-side}.png` +
`trees.json`), and the tables in GR1.md §5.
