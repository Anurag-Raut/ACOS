# PHONE2 — handoff (arm64 Gaten f344272 reference)

Branch `phone-ref-2` (base `int-nhp` = `f6c418d`, i.e. `main` + NET1 + H13B + PHONE-REF-1).
Full report also in `EXPERIMENTS.md` E13.15. Continues PHONE-REF-1 (`results/handoff/PHONE.md`).

## 1. What this track delivered

1. **The Gaten build mismatch is gone.** PHONE-REF-1 was blocked because the phone ran a
   different Gaten build than ACOS tests. This track built Gaten `dev` `f344272` for
   **arm64-v8a exactly the way ACOS's x86_64 build was made**, signed it with a debug keystore,
   installed it on the moto g35, and proved the **JS bundle is byte-identical** to the one
   ACOS runs. Both sides now execute the same app code; the only remaining differences are
   platform ones.
2. **All 10 Gaten scenarios + their 9 negative twins replayed on the phone** with per-step
   screenshot + tree evidence under `results/phone/gaten-f344272/<scenario>/`.
3. **Three concrete divergences classified**, one of them a *candidate ACOS false pass* on
   text-entry fields (§5.1).
4. **Harness fixes** in `tools/phone/replay_scenario.py` (§6).

## 2. Build (gaten `f344272`, arm64)

Procedure followed from `results/gaten/G1.md` §1.2. Clone
`/home/Qui/Projects/acos/.cache/gaten/src-dev` was already at
`f3442722278d7dcf7c584df1b09b348b04f0b1f2`; `/home/Qui/Projects/gaten` was never written.

```
tools/guarded.sh --mem 12G --time 1500 -- bash -c 'cd …/src-dev &&
  ANDROID_HOME=/home/Qui/Android/Sdk ./gradlew assembleRelease \
  -PreactNativeArchitectures=arm64-v8a --max-workers=4 --no-daemon'
```

| artifact | size | sha256 |
|---|---|---|
| `app-arm64-v8a-release-unsigned.apk` (clone `app/build/outputs/apk/release/`) | 112,392,697 B | `130678ce26529f11cb1f0db9c65a4e5648fedfd6422f3be52d7caffc8d3554be` |
| **`/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-arm64-release-debugsigned.apk`** (kept, installed) | 112,483,404 B | `e0d491cf4d8f79d9e6fdf76cdd09716bd9d6c30e419183c70152e2ae4357d091` |
| ACOS's x86_64 build (`gaten-dev-f344272-x86_64-release-unsigned.apk`, unchanged) | 114,751,344 B | `e04c7ffcf01209c98e511e1ab01a7f68d6decb178d14a08f2b7f0a6539e2ee29` |

* Signing: `apksigner` (build-tools 34.0.0) with a fresh debug keystore at
  `.cache/phone2/debug.keystore`; scheme **v3**, signer cert SHA-256
  `a79082778ad8605a741eef1642c1ba1394b8b6f4ee311ff17a6b6ea86e8c3e1f`.
* Manifest (`aapt2 dump badging`): `com.gaten`, **versionCode 12, versionName 0.1.0**,
  minSdk 29, targetSdk 36, `native-code: 'arm64-v8a'` only, **23** arm64 `.so`.
* **JS bundle identical**: `assets/index.android.bundle` sha256
  `3c5428ea05ac2e9a90ccfef42a3c47e850656fd4744b0febaadde747d6d2300b`, 3,565,652 B — the same
  value in the arm64 APK, a freshly rebuilt x86_64 APK, **and** the x86_64 APK ACOS has been
  testing. `:app:createBundleReleaseJsAndAssets` was `UP-TO-DATE` (same source), so this is a
  real equality check, not a coincidence of two empty builds.
* The Gradle run was killed by the guard at the very end of `:app:assembleRelease`
  (exit 137, `MemoryMax=12G`) **after** the APK was written: `unzip -t` passes, all 23 libs and
  the bundle are present, and the rebuilt x86_64 APK from the same run is 1 MB *smaller* than
  G1's, which is why the bundle comparison above was done rather than trusting sizes.

## 3. Install (moto g35, `ZA222WGJPP`)

`flock .cache/phone.lock` held for every adb/agent-device action.

```
adb -s ZA222WGJPP uninstall com.gaten        → Success
adb -s ZA222WGJPP install -r <arm64 apk>     → Success (incremental, 9.5 s)
dumpsys package com.gaten → versionCode=12 versionName=0.1.0
                            lastUpdateTime=2026-11-14 16:59:49
                            signatures=[f1b9491e]   (the old install's were different)
```

`versionCode`/`versionName` are unchanged from the build that was replaced (Gaten derives the
code as `base*10 + abi`, so every arm64 build is 12), so the **install timestamp and the new
signer digest** are the evidence that the phone runs this build. The visible proof is the UI:
the old install showed the coach-mark tour with no `Skip`; this build shows
`Set up Gaten` + three `Skip` pages, which is exactly what all five `gaten-*` scenarios step
through (`results/phone/gaten-f344272/gaten-home/phone/screenshot-01-tap.png`).

Device under test: `motorola/manila_g/manila:15/VUOA35.116-151/a43dc2:user/release-keys`,
API 35, **1080x2400 @ 400 dpi**, font_scale 1.0, arm64. The scenarios declare
`display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 400}`, so the phone's
geometry is the scenario's geometry (`ACOSLAUNCH_DENSITY` is 420 for ACOS's *reference*
session, which is where the systematic bounds offset in §4 comes from).

## 4. Scenario results: phone vs ACOS

Both sides ran the **same app build** (§2) and the same scenario file. ACOS verdicts are fresh
runs from this track (`--engine art`, API 34, x86_64) — every one of the nine is `pass`.

| scenario | steps | ACOS | phone | phone-only difference |
|---|---|---|---|---|
| `gaten-home` | 14 | 14/14 pass | **14/14 pass** | Anki card state (§5.2); bounds (§5.3) |
| `gaten-tabs` | 22 | 22/22 pass | 21/22 | step 12 `Anki is not installed.` — the card reads *Permission needed* (§5.2) |
| `gaten-settings` | 33 | 33/33 pass | **33/33 pass** | bounds only |
| `gaten-search` | 22 | 22/22 pass | 13/22 | **step 7: the search field's accessible name (§5.1)**; steps 11–21 cascade |
| `gaten-lesson` | 28 | 28/28 pass | 15/28 | step 7 Anki card label (§5.2); `back` then exits the app → 0-node tail |
| `gaten-support` | 27 | 27/27 pass | **27/27 pass** | none |
| `gaten-kanji` | 33 | 33/33 pass | **33/33 pass** | none |
| `gaten-recognition` | 33 | 33/33 pass | **33/33 pass** | none |
| `gaten-backstack` | 25 | 25/25 pass | 22/25 | **steps 13/15: the token field's name (§5.1)**, step 18 `Save`; flow recovers, later steps pass |

**Six of nine are step-for-step identical in status**; the three that are not differ only on the
text-field name (§5.1) and the Anki device state (§5.2).

**Negative twins** (`scenarios/negative/gaten-*-wrong`, one per scenario) — both sides must fail:

| twin | ACOS first fail | phone first fail |
|---|---|---|
| `gaten-home-wrong` | 7 | 7 |
| `gaten-tabs-wrong` | 7 | 7 (+12, the Anki card) |
| `gaten-settings-wrong` | 12 | 12 |
| `gaten-kanji-wrong` | 19 | 19 |
| `gaten-recognition-wrong` | 28 | 28 |
| `gaten-support-wrong` | 13 | 13 |
| `gaten-search-wrong` | 16 | 7 — the field-label step (§5.1) fails first |
| `gaten-lesson-wrong` | 10 | 7 — the Anki card (§5.2) fails first |
| `gaten-backstack-wrong` | 23 | 13 — the token field (§5.1) fails first |

**All nine fail on both sides**; six at the same step, and the three whose positive twin is affected
by §5.1/§5.2 fail *earlier* on the phone — the wrongly-asserted step still fails, so a twin never
turns into a pass.

Evidence: `results/phone/gaten-f344272/<scenario>/phone/observations.json` +
`phone/screenshot-NN-*.png` (360 px wide, 256-colour PNG), the ACOS side in `<scenario>/acos/observations.json`
(normalized from `.cache/phone2/acos/<scenario>/`), and the mechanical per-step diff in
`<scenario>/DIFF.md` (`tools/phone/compare_observations.py`). Its classes: `bounds` (pixel
growth only, §5.3), `text-set` (the two trees carry different label sets), `state` (the resolved
node's own state/identity differs), `missing-on-phone`, `status`.

### 5. Differences, classified

**5.1 (a) CONFIRMED ACOS FALSE PASS — a text field's accessible name.** The two text inputs
resolve on ACOS and **do not exist on the device**:

| field | scenario selector (resolves on ACOS) | the phone's tree |
|---|---|---|
| Home search | `content_desc: "Search Japanese dictionary"` | `猫, ねこ, neko...` (the placeholder; no `text`) |
| WaniKani token | `content_desc: "Personal access token"` / `"WaniKani personal access token"` | `Paste your token` (the placeholder) |

Same APK, same JS bundle, so this is a tree-provenance difference, not app behaviour. ACOS's
tree carries **both** the RN `accessibilityLabel` (`tree` step 7 target `content_desc =
"Search Japanese dictionary"`) **and** the placeholder as a separate `text` node
(`猫, ねこ, neko...`); the real Android accessibility bridge gives the `EditText` **only the
hint** as its accessible name and no text.

Consequence: `gaten-search` passes 22/22 on ACOS and **13/22 on the phone** — the tap at step 7
is `ELEMENT_NOT_FOUND`, the `type` then has nothing focused, and 9 steps fall over
(`results/phone/gaten-f344272/gaten-search/DIFF.md`, `…/phone/observations.json` step 7;
ACOS's own resolving node is in `…/acos/observations.json` step 7). Same class in
`gaten-backstack` steps 13/15 and `gaten-lesson` steps 19/20/23. This is the one difference that
constitutes a **false pass on the ACOS side**: a scenario naming a field by its
`accessibilityLabel` verifies green on ACOS and cannot be reproduced on a phone.

**5.2 (b) phone-only, owner device state — the Anki card.** The owner's phone **has
AnkiDroid installed** (`pm list packages` → `com.ichi2.anki`). Gaten therefore shows
`Anki, Permission needed, Grant access` / *"Anki access needed. Allow Gaten to read
collection statistics."*, where the scenarios assert the *not-installed* branch
(`Anki is not installed.` / `Download Anki`, `Anki, Not connected`). ACOS has no AnkiDroid, so
it takes the other branch. **Not an ACOS bug**; it is a scenario assumption (the flow is
device-state dependent). It costs `gaten-tabs` 1 step and `gaten-lesson` 13 (the first failure
is step 7; the `back` at step 12 then exits the app, so every later step reads an empty tree —
see the `node_count` column in that file).

**5.3 (b) phone-only, geometry — every bounds row.** ACOS's reference session boots at
density 420 and the phone is 400 dpi, so every comparable node's bounds differ by 5–60 px
(`gaten-home` steps 2–9: ACOS `[498,2163][582,2209]` vs phone `[500,2175][580,2225]`).
The scenario asks for 400, so this is ACOS's reference-density default (already filed as G4b
in E13.9), not a phone artifact.

**5.4 (b) phone-only, accessibility shape — merged container labels.** On the device, a
Fabric container node merges its descendants' labels into one `content_desc`
(`Anki, Collection, Anki access needed., …, Accuracy, …, Study activity, …`), so a
`text contains "Accuracy"` selector resolves to the **container** (whole-screen bounds) where
ACOS resolves the leaf. `compare_observations.py` classes these as `state`; for an agent it
means a text selector can hit the container centre. Same app, different tree granularity.

**5.5 (b) harness, now fixed — a dozing display reads an empty tree.** The first phone run of
this track produced 0 nodes and black screenshots at every step: the device had dozed while a
phone-lock wait ran. Every node then reported `visible=false`, which the harness turns into
`ELEMENT_NOT_FOUND`. Real-phone harnesses must wake the device; `replay_scenario.py` now does
(§6). This is a harness bug, **not** an app or ACOS difference.

**5.6 (b) harness, now fixed — `assert_visible` was unimplemented** (`unsupported_step`), so
`gaten-kanji`, `gaten-recognition`, `gaten-support` could not report a verdict even when the
screen was right. Implemented; those three now pass 33/33, 33/33, 27/27.

**5.7 (d) none / no ACOS behaviour defect found** on the flows where both sides ran the same
APK — apart from §5.1, which is a tree-provenance difference rather than a wrong ACOS
verdict for the selector it was given.

## 6. Harness changes (`tools/phone/replay_scenario.py`)

* wake + keyguard-dismiss + `svc power stayon` for the duration of a replay, restoring
  `stay_on_while_plugged_in` afterwards (fixes §5.5);
* phone captures re-encoded to 360 px, 256 colours (the device captures at 1080x2400, ~650 KB each;
  a full 19-scenario reference is then 27 MB instead of 88 MB);
* `assert_visible` implemented alongside `assert_not_visible` (fixes §5.6).

## 7. What remains

1. **Decide the fix for §5.1 (the only blocker).** Either ACOS's tree should expose an input's
   `hint` the way the platform does (and keep `accessibilityLabel` as an extra signal), or the
   scenarios should target the placeholder (`猫, ねこ, neko...`, `Paste your token`) so the flow
   passes on both sides. Naming the field by what a device exposes makes the pass honest.
2. **Re-baseline the Anki-dependent steps** (§5.2): they assert a device state, not app
   behaviour. Either express both branches or run them on a device without AnkiDroid.
3. ACOS reference density (§5.3) is G4b, already open; it is the only source of the `bounds`
   rows.
4. Optional: teach `compare_observations.py` the `state` class's merged-container case (§5.4)
   so those rows read as "tree granularity" instead of "state".
5. Run the 9 negatives on the phone once more after (1) — they are device-state sensitive in the
   same way as their positive twins (`gaten-tabs-wrong` fails at step 7 on both sides, then the
   phone fails step 12 where ACOS does not).
