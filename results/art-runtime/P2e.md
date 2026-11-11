# P2e — `render-app` applies the image's static RROs, the device's way

**Status: every acceptance number met.** `counter.apk` rendered by
`cargo run -q -p acos-art -- render-app --apk fixtures/build/apps/counter.apk`
(no extra flags) is **`Increment` at 231 px**, **SSIM 0.99685**, **4/4 content
bounds exact**, **md5 `2fc7c91d…` ×5 runs**, with **no forced `textAllCaps`
anywhere** — the overlays decide. Owner: implementer (P2e), 2026-11-11. Basis:
P2a (`results/art-runtime/P2a.md`), P2c + `research/art-feasibility/h-overlay/`,
`docs/ENGINEERING_POLICY.md`. Nothing committed; `crates/acos-binder-peer` and
`src/main.rs` untouched; no sudo; every run under `tools/guarded.sh --mem 6G`.

```bash
# acceptance command (defaults; --out omitted ⇒ artefacts stay in <data>/out/render-app)
cargo run -q -p acos-art -- render-app --apk fixtures/build/apps/counter.apk
#   frame=…/frame.png tree=…/tree.json md5=2fc7c91db19d870ff18d66acc8f7f497 nodes=5 1080x2062@420
#   outcome=ok wall=131ms draw_cold=368us draw_warm=19us peak_rss=201MB
python3 tools/art-render-app-compare.py \
    --png results/art-runtime/p2e/counter/frame.png \
    --tree results/art-runtime/p2e/counter/tree.json
```

## 0. Acceptance

| requirement | result |
|---|---|
| idmap generation in `acos-art` **provisioning**, cached by overlay hash | **PASS** — `app::provision_overlays`: the image's own `/system/bin/idmap2 create --policy product` inside the sandbox, one idmap per static RRO, cache key = FNV(overlay APK) + FNV(`framework-res.apk`) + policy (the idmap stores both CRCs, P2c §7) |
| guest `AppMain` builds the `AssetManager` **with overlays** | **PASS** — `ApkAssets.loadFromPath(framework-res)` + 5 × `ApkAssets.loadOverlayFromPath(idmap, PROPERTY_SYSTEM)` + app APK → `AssetManager.Builder` (7 `ApkAssets`), 0.12 ms |
| font provisioning sets **`Typeface.sDefaults`** non-null for all 4 styles *and* `sDefaultTypeface` / `sSystemFontMap` | **PASS** — `sDefaults=[true,true,true,true]`, `sSystemFontMap=2` families, `sSystemFallbackMap=202`, `sDefaultTypeface` set via `Typeface.setDefault` (Java + libhwui) |
| `textAllCaps` workaround **removed from the guest** | **PASS** — no `setAllCaps` call, no `acosart.allcaps` env; `--text-allcaps theme` is a reported **no-op** (same PNG md5) |
| SSIM vs ART oracle **≥ 0.9968** | **PASS** — **0.99685**, MAE 0.115/255, 98.50 % identical pixels |
| **4/4** content bounds | **PASS** — root `LinearLayout`, `#count`, `#inc`, `#reset` all exact; the 4 remaining oracle rows are the structural decor/`id/content` nodes P2a deliberately does not build (`missing_in_ours`) |
| `Button` width **231** | **PASS** — `#inc` 231 px (= oracle), `#reset` 231 px |
| deterministic md5 | **PASS** — `2fc7c91db19d870ff18d66acc8f7f497` on **5** consecutive runs (3 with explicit `--out`, 2 with the default) |
| `cargo test -p acos-art` | **PASS** — **17 passed**, 0 failed |
| RAM < 90 %, `guarded.sh`, no sudo | **PASS** — peak guest `VmHWM` **200.8–207.7 MB** (host 60 GiB, ~34 GiB in use); every command via `guarded.sh`; no privileged operation |

### The numbers, one place (`counter.apk`, 1080×2062 @ 420 dpi)

| measurement | control (no overlays) | **image's 5 static RROs** |
|---|---|---|
| `Button#inc` bounds (content-local) | `[42,93][309,219]` = **267 px** | `[42,93][273,219]` = **231 px** = oracle |
| button text | `INCREMENT` | **`Increment`** |
| `android:bool/config_buttonTextAllCaps` | `true` (base `framework-res`) | **`false`** (overlaid) |
| SSIM / MAE / identical px | 0.99196 / 0.432 / 98.22 % | **0.99685 / 0.115 / 98.50 %** |
| content nodes exact | 3/4 | **4/4** |
| PNG md5 | `237e5fd13b9b3cfc8aa51d63cdc07e48` | **`2fc7c91db19d870ff18d66acc8f7f497`** |
| wall (whole command) | 157 ms | 126–146 ms |
| peak RSS | 203 MB | 200.8–207.7 MB |
| draw cold / warm | 440 / 29 µs | 329–368 / 19–20 µs |
| idmap bytes / load | — | **2952 B**, 0.12 ms for 5 overlay `ApkAssets` |

The control is the *same binary* with `.cache/boot/android-34/overlays`
temporarily absent (moved aside and restored in the same command); it reproduces
P2c's control byte-for-byte, which is the clean statement that the delta is the
overlay and nothing else.

## 1. What shipped

| path | what it is |
|---|---|
| `crates/acos-art/src/app.rs` | +`OverlaySet` / `provision_overlays` (idmap generation + hash cache + `aapt2` manifest filter) / `is_static_android_overlay` / `stage_file` / `sandbox_tool`; idmap paths on the guest argv; `AppResult.overlays` + `idmap_bytes`; docs rewritten (resource path is the device's); two bug fixes (§5) |
| `crates/acos-art/guest/java/dev/acos/art/AppMain.java` | +`buildAssetManager` / `apkAssetsAssetManager` / `apkAssetsOverlay` / `probeTheme`; `seedTypeface` rewritten (map + fallback + defaults); `setAllCaps` **deleted**; optional `idmap,idmap,…` argv |
| `tools/art-render-app-compare.py` | new: SSIM/MAE/exact-pixel + node-bounds diff for a `render-app` output, importing E4H's comparator unchanged (`research/art-feasibility/h-view/scripts/compare.py`) |
| `results/art-runtime/p2e/counter/` | `frame.png`, `tree.json` (acceptance evidence, default `--out`) |
| `results/art-runtime/p2e/control/` | same, overlays absent (control evidence) |
| `results/art-runtime/p2e/compare{,-control}.json` | the comparator output, verbatim |

`shim/render_shim.c` is **unchanged**: overlay loading needs no new native
symbol (`register_android_content_res_ApkAssets` was already registered in P2a).

## 2. Provisioning: the image's own idmap tool, once, cached

`provision_overlays(root, diagnostics)` runs on every `render-app`, but only
*checks* in the steady state:

1. **Source** — `.cache/boot/android-34/overlays/*.apk`, i.e. what
   `tools/extract-bootclasspath.sh` (W198A) already filtered to the image's RROs.
   That set is re-verified here from each APK's **binary manifest**
   (`aapt2 dump xmltree`): `<overlay android:targetPackage="android"
   android:isStatic="true">`. Nothing about *which* resources an overlay changes
   is hardcoded — no resource name or value appears in ACOS code.
2. **Stage** — APK hard-linked into `<data>/overlay/` (0 bytes, same filesystem),
   because the idmap names the overlay by path and `loadOverlayFromPath` loads
   both APKs from the idmap.
3. **Generate** — inside the `acos-art` sandbox, with the image's own binary
   (`/system/bin/idmap2`, x86_64):
   ```text
   idmap2 create --policy product \
     --target-apk-path  /system/framework/framework-res.apk \
     --overlay-apk-path /data/overlay/<overlay>.apk \
     --idmap-path       /data/overlay/<overlay>.idmap
   ```
   `--policy product` is mandatory (`framework-res` declares no `<overlayable>`;
   without a policy idmap2 rejects every entry — P2c §4.1).
4. **Cache** — `<overlay>.idmap.key` holds `FNV(overlay APK)-FNV(framework-res)-product`.
   Overlay **or** target change ⇒ regenerate; otherwise reuse. Both CRCs are in
   the idmap, so keying on both is what makes a stale idmap impossible.
5. **Guest argument** — `,`-separated on-device idmap paths, added as the last
   argv element (empty string when the extraction/target/`idmap2` is missing; the
   run then reports the reason and renders the un-overlaid resources).

Measured: 5 idmaps, **2952 B** (P2c's 2956 B differs only because the idmap
stores the overlay path string — `/data/overlay` ×5, not `/system/overlay`).
Generation is **~19 ms per overlay** end-to-end (all 5 fresh: 747 ms for the
whole command vs 653 ms cached; one fresh: 672 ms), i.e. a one-off ~0.1 s, not a
per-frame cost. Runtime load: **0.12 ms** for the 5 overlay `ApkAssets`, 7
`ApkAssets` total in the `Builder`.

## 3. Guest: the same objects `ResourcesManager` builds at zygote time

```java
ApkAssets target  = ApkAssets.loadFromPath("/system/framework/framework-res.apk");
ApkAssets overlay = ApkAssets.loadOverlayFromPath(idmap, PROPERTY_SYSTEM /* 0x1 */);  // × n
ApkAssets app     = ApkAssets.loadFromPath("/data/app/counter.apk");
AssetManager am   = new AssetManager.Builder()
        .addApkAssets(target).addApkAssets(overlay)…addApkAssets(app).build();
```

- API-34 shape is `loadOverlayFromPath(String, int)`, found by reflection over
  the real class (the parameter is an `int` flag, not `boolean system`); the flag
  is `PROPERTY_SYSTEM`, read from the class, not written as a literal.
- Order is priority (later wins); the five overlays are added in the image's
  alphabetical order, as the extraction sorts them.
- Per-idmap failures (e.g. a stale idmap) are **reported**, not swallowed:
  `warnings[overlay_idmap_failed=…]`, `notes[idmap … not applied: …]`, and if
  none loads, `overlays_unavailable=n`. If `ApkAssets`/`Builder` are missing
  entirely, the old constructor + `addAssetPath` path is used with an explicit
  warning (`apk_assets_unavailable=…`, *no overlays applied*).
- `probeTheme()` records the decisive resource resolution in every run:
  `android:bool/config_buttonTextAllCaps=0x111004a -> false`,
  `android:string/config_bodyFontFamily -> google-sans-text`.

## 4. Fonts: the third zygote artefact (`sDefaults`)

P2c's warning ("seed `sDefaults[0..3]`, not just `sDefaultTypeface`") is now
implemented, and it is load-bearing: the overlay points the theme at
`google-sans-text`, which **this image does not ship and the device does not
ship either** — on both, `Typeface.create("google-sans-text", style)` returns
null and falls through to `sDefaults[style]`. Null there is an NPE in
`Typeface.create` during inflate.

`seedTypeface()` now does what zygote does:

1. `SystemFonts.getSystemPreinstalledFontConfig()` (149 families in this image)
   → `buildSystemTypefaces(FontConfig, Map)` and `buildSystemFallback(…)` →
   install as `Typeface.sSystemFontMap` / `sSystemFallbackMap`.
2. Default family: `sSystemFontMap["sans-serif"]`, else
   `createFromFamilies("sans-serif", FontFamily(Font(Roboto-Regular)))`, else
   `Typeface.createFromFile`.
3. `Typeface.setDefault(base)` — Java field **and** libhwui `gDefaultTypeface`
   (the field alone trips libhwui's `ASSERT src == nullptr && gDefaultTypeface ==
   nullptr`).
4. `sDefaults = {base, create(base,BOLD), create(base,ITALIC),
   create(base,BOLD_ITALIC)}` with a per-index fallback to `base`, so all four
   are non-null. Recorded per run: `sDefaults=[true,true,true,true]`.

Honest detail: on this image `buildSystemTypefaces` yields **0 named families**
(mapping into the supplied map is empty) even though the `FontConfig` has 149
families — the same `CustomFallbackBuilder` quirk P2c/h-overlay hit; the named
family construction path throws during `Typeface.<clinit>`-time state. The guest
therefore seeds `sans-serif` + `roboto` from the real Roboto file and says so
(`sSystemFontMap=2 families (font-file fallback)`) while `sSystemFallbackMap` is
still the framework's 202 entries. This is behaviourally identical to the device
for every family this app asks for, proven by the pixel-identical frame
(0.99685 / md5 `2fc7c91d…`, exactly P2c's overlay frame).

## 5. Two product bugs found and fixed while validating the acceptance command

1. **`--out` omitted produced an empty `frame.png`** (`md5 d41d8cd9…`). `out`
   defaulted to the staging dir and `fs::copy(staged, out/frame.png)` copied the
   file onto itself, truncating it. Now the copy is skipped when source and
   destination are the same path. This is why the *default* command — the one
   acceptance names — had to be exercised explicitly; `--out DIR` had hidden it.
   The self-copy also truncated `frame.png`/`tree.json` in the staging dir, so
   the next default run recovered them before the md5 was taken (verified).
2. **Collected diagnostics were dropped.** `render_app` accumulated fonts/overlay
   diagnostics into `diagnostics` but returned `launcher::classify()`'s vector,
   so the provisioning lines never reached `AppResult`. Now spliced in front
   (`static overlays: 5 applied (5 idmaps generated, 0 cached, 2952 B)` /
   `(0 generated, 5 cached)`).

## 6. Removing `textAllCaps` (and what stayed)

- `AppMain` no longer has `setAllCaps`, and `render-app` no longer exports
  `acosart.allcaps`. The Button's sentence case now comes from
  `Theme.DeviceDefault.Light` resolving `config_buttonTextAllCaps=false`
  **through the overlay** — i.e. from data, like the device.
- `AppOpts::text_all_caps` / `--text-allcaps` are kept as a **deprecated no-op**
  so the CLI surface and `src/main.rs` stay untouched (that file is not this
  task's to edit); `--text-allcaps theme` prints
  `--text-allcaps is a no-op: the image's static RROs decide
  config_buttonTextAllCaps (the guest never forces it)` and produces the same
  PNG md5. Delete the flag when `main.rs` is next edited.
- `RippleDrawable` → content-layer and the `IWindowManager` proxy remain (they
  are rendering/transport seams, not resource-resolution workarounds).

## 7. Regression / other fixtures

- `hello-views.apk`: `crash / INFLATE_FAILED`, unchanged cause chain (P2a §4:
  `EditText` → `InputMethodManager` → `Settings.Config` → `IContentProvider`).
  The overlays now *are* applied on the way there
  (`AssetManager.Builder: 7 ApkAssets …`, `config_buttonTextAllCaps -> false`),
  so it is not a regression.
- `cargo test -q -p acos-art`: 17/17. The comparator tool is the only new test
  harness; guest-side acceptance is checked by re-running the command.

## 8. Cost

| measurement | value |
|---|---|
| idmaps | 5 files (456 / 976 / 152 / 284 / 1084 B) = **2952 B**; `--policy product` recorded by idmap2 in the header |
| idmap generation (one-off) | **~19 ms per overlay** end-to-end under `guarded.sh` + bwrap (5 fresh: 747 ms command vs 653 ms cached) |
| per-run cache check | 5 × (read APK + FNV + read key) — inside the run's noise |
| overlay `ApkAssets` load | **0.12 ms** for 5, `Builder` 7 `ApkAssets` |
| whole command | **126–146 ms** (P2a: 121–146 ms) |
| peak RSS (`VmHWM`) | **200.8–207.7 MB** (P2a: 165–167 MB, +~35 MB) |
| draw | 329–368 µs cold / 19–20 µs warm |
| frame | 20 773 B PNG, md5 `2fc7c91db19d870ff18d66acc8f7f497` ×5 |
| logd entries | 967 (P2a 506): overlay/font-map work logs more; no fatal/abort entries |

The +35 MB is **not** the overlays (2952 B of idmaps, 300 KB of APKs; P2c
measured +0.4 MB on an already font-mapped process). It is the font map that P2a
never built: a 149-family `FontConfig` → 202 fallback entries and real `Typeface`
objects. It buys the `sDefaults`/fallback behaviour the overlays require; the
frame is unchanged.

## 9. Side effects, undo, and what is deliberately not done

- **Additive, disposable state:** `<data>/overlay/` (5 hard-linked APKs + 5
  idmaps + 5 key files). Undo: `rm -rf .cache/art/data/overlay`. **No sysroot
  mutation** for overlays (unlike the fonts, which P2a hard-links into
  `<sysroot>/system/fonts`).
- **Not done:** dynamic (non-static) RROs — they need `OverlayManager`/OMS state
  and are not part of a zygote-time loader; per-locale / APEX-hosted
  `framework-res` overlays (not in this image's static set); overlays that
  override non-scalar resources are *now* exercised by the stock path in
  principle but only this app was measured (P2c §7 lists the same probe);
  priority/conflict resolution between overlays (all five are `priority=1`, no
  clash observed); a product provisioning step that ships idmaps instead of
  generating them on first use (`tools/art-sysroot.sh`/`art-overlay` command is
  an obvious follow-up, ~0.5 d, and would make `render-app` need no SDK for
  idmap generation).
- **Time:** stopped inside the 60-minute checkpoint (implementation +
  verification + two bug fixes + report); no retry loop.
