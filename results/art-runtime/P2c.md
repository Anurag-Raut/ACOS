# P2c — the image's static RROs, applied on the **stock** ART resource path

**Answer: yes, and with no ACOS resource code and no hardcoded attribute.** On the
stock path (`libandroid_runtime`'s `AssetManager` / `ApkAssets`, real idmaps, real
`libandroidfw`) the `counter.apk` `Button` now renders **`Increment`** (231 px wide,
the ART oracle's width) instead of `INCREMENT` (267 px). SSIM against the ART oracle
moves **0.99196 → 0.99685** (MAE 0.43 → 0.115 / 255, exact pixels 98.22 → 98.50 %)
— the same jump E4H got only by hardcoding `textAllCaps=false` on the `TextView`s,
and now obtained by loading the overlay APK the image itself applies.

| | control (stock path, no overlay) | + the image's static RROs |
|---|---|---|
| `Theme.DeviceDefault.Light` → `bool/config_buttonTextAllCaps` | `true` (base `framework-res`) | **`false` (overlaid)** |
| `Button#inc` bounds (content-local) | `[42,93][309,219]` = **267 px** | `[42,93][273,219]` = **231 px** = ART oracle |
| SSIM vs ART oracle | 0.99196 | **0.99685** |
| MAE | 0.43 / 255 | **0.115 / 255** |
| identical pixels | 98.22 % | **98.50 %** |
| content nodes exact (4) | 3/4 | **4/4** |
| PNG md5 (stable across runs) | `237e5fd13b9b3cfc8aa51d63cdc07e48` (= E4H's) | `2fc7c91db19d870ff18d66acc8f7f497` |
| wall / peak RSS | 266 ms / 251.4 MB | 261 ms / 251.8 MB |

- **Owner / date:** experimenter agent, 2026-11-11. Hard checkpoint 60 min; used ~55.
- **Scope honoured:** no ACOS crate touched (`crates/**` untouched, nothing built from
  them), nothing committed, no sudo, every run inside `tools/guarded.sh`, new files only
  under `research/art-feasibility/h-overlay/` + this report. Scratch additions are
  *additive* to the E1A sysroot (`<sysroot>/system/overlay/*.apk`,
  `.cache/art-exp-h/**`) — undo with `rm -rf .cache/art-exp-h` and
  `rm -rf .cache/art-exp/sysroot/system/overlay`.
- **Depends on:** E4H (`research/art-feasibility/h-view/` — the harness is a fork of its
  ladder), E1A sysroot + `artroot.sh`, E2D `libbindershim.so`, and W198A's extraction of
  the five static overlays (`.cache/boot/android-34/overlays/`, `tools/extract-bootclasspath.sh`).

```bash
research/art-feasibility/h-overlay/scripts/build.sh
research/art-feasibility/h-overlay/scripts/run.sh all        # overlays applied
HOV_OVERLAY= research/art-feasibility/h-overlay/scripts/run.sh all   # control
python3 research/art-feasibility/h-overlay/scripts/compare.py
```

## 0. The three answers the brief asked for

1. **Which overlays.** All **five** `isStatic=true, targetPackage="android"` RROs the
   API-34 image ships in `product/overlay` (same set W198A identified for ACOS-VM):

   | APK (from `product/overlay/`) | package | idmap mappings | idmap bytes |
   |---|---|---|---|
   | `framework-res__auto_generated_rro_product.apk` | `android.auto_generated_rro_product__` | 13 | 460 |
   | `GoogleConfigOverlay.apk` | `com.google.android.overlay.googleconfig` | 90 | 976 |
   | `GoogleWebViewOverlay.apk` | `com.google.android.overlay.googlewebview` | 1 | 152 |
   | `LargeScreenConfigOverlay.apk` | `com.google.android.overlay.largescreenconfig` | 17 | 284 |
   | **`PixelConfigOverlayCommon.apk`** | `com.google.android.overlay.pixelconfigcommon` | **111** | 1084 |

   Only the last one matters for this app: its idmap contains
   `0x0111004a -> 0x7f020001 (bool/config_buttonTextAllCaps -> bool/config_buttonTextAllCaps)`.
   Applying the other four changes **nothing** here — the 5-overlay PNG is
   byte-identical (md5 `2fc7c91d…`) to a run with `PixelConfigOverlayCommon` alone.

2. **How loaded.** The idmaps are generated *offline* by the **image's own
   `/system/bin/idmap2`**, then loaded exactly the way `ResourcesManager` does it at
   zygote time — one `ApkAssets` per overlay, `idmap` attached by
   `ApkAssets.loadOverlayFromPath(idmapPath, loadFlags)`, all of it handed to
   `AssetManager.Builder`:

   ```java
   ApkAssets host    = ApkAssets.loadFromPath("/system/framework/framework-res.apk");
   ApkAssets overlay = ApkAssets.loadOverlayFromPath("/data/overlay/PixelConfigOverlayCommon.idmap",
                                                     ApkAssets.PROPERTY_SYSTEM /* 0x1 */);
   ApkAssets app     = ApkAssets.loadFromPath("/data/apk/counter.apk");
   AssetManager am = new AssetManager.Builder()
           .addApkAssets(host).addApkAssets(overlay).addApkAssets(app).build();
   ```

   API shape (found by reflection, not guessed): API 34 has
   `ApkAssets.loadOverlayFromPath(String idmapPath, int loadFlags)` — **`int`
   flags, not a `boolean system`** — with `FORMAT_{APK=0,IDMAP=1,ARSC=2,DIR=3}` and
   `PROPERTY_{SYSTEM=1,DYNAMIC=2,LOADER=4,OVERLAY=8,DISABLE_INCREMENTAL_HARDENING=16}`.
   `loadFlags` only marks provenance: `0x0` and `0x1` both apply the overlay
   (measured, identical output). If `ApkAssets`/`Builder` are absent (they are public
   but `@hide`-ish) the fallback is `ApkAssets` + the private `AssetManager`
   constructor + `nativeSetApkAssets` — not needed here.

3. **Cost.** Runtime: **1.19 ms** to load target + 5 overlays, **0.06 ms** for
   `Builder.build()`, **+0 ms wall** (261 vs 266 ms control, i.e. inside noise),
   **+0.4 MB peak RSS** (251.8 vs 251.4 MB). Provisioning (one-off, offline):
   5 idmaps, **2956 bytes total**, ~0.37 s per `idmap2` invocation end-to-end
   (all of that is bwrap start-up; the tool itself is below the measurement floor of
   8 ms).

## 1. Where the overlay set comes from (no new extraction)

`tools/extract-bootclasspath.sh` (W198A, §1.4 of `results/parity/W198A.md`) already
`lpunpack`s `product`/`system_ext` and copies only `isStatic && targetPackage=="android"`
into `.cache/boot/android-34/overlays/` — exactly 5 files, rejecting 20 dynamic or
app-targeted overlays. This experiment reuses those five **as-is**; it does not touch
the extraction. Nothing in the harness hardcodes a resource name or value: the only
string literal is the overlay *file's* name, in `run.sh`.

## 2. The stock overlay plumbing, in detail

`ApkAssets.loadOverlayFromPath` reads the idmap (magic `IDMP`, version 9, target path,
overlay path, target/overlay CRCs, `mapping`, debug string pool, policy flag) and
loads **both** APKs the idmap names, so the overlay APK never has to be added to the
`AssetManager` separately and the base `framework-res.apk` is the same file, at the
same path, as without the overlay. `AssetManager.Builder.build()` →
`nativeSetApkAssets(mObject, apkAssets, false)`; `libandroidfw`'s
`AssetManager2::SetApkAssets` then registers each overlay's `Idmap` against its
target's package id, which is what makes `ResTable::resolve` return the overlay's
`Res_value` for `0x0111004a` while keeping the *target's* resource name/id.

Two details worth carrying into the product:

- **The Java side is happy with an overlay `ApkAssets` in the array** (unlike
  ACOS-VM's hand-written manager, where the equivalent had to be kept *out* of
  `sources` because `getPooledStringForCookie` indexed the Java-side string-block
  cache — W198A §1.3). On the stock path cookies are `1..n` over **all** `n`
  `ApkAssets`, overlays included (app APK at index 3 with one overlay, 7 with five),
  and `getIdentifier/getString/getLayout` all resolve normally.
- **Order is priority**: later `addApkAssets` wins. The five overlays all declare
  `android:priority=1`, so we add them in the framework's order (alphabetical, as the
  extraction sorts them). No conflict was observable on this app.

## 3. Result

`artifacts/compare.txt` (verbatim) — the harness now reports the *same* numbers with
and without its `textAllCaps=false` variant, i.e. the theme itself says sentence case:

```
hov-view.png        art-oracle SSIM=0.99685 MAE=0.115 identical_pixels=98.50%
hov-view-nocaps.png art-oracle SSIM=0.99685 MAE=0.114 identical_pixels=98.50%
```

`artifacts/hov-tree.txt` vs the oracle (content nodes, device bounds minus content
origin y−275):

| node | device (ART) | ours, no overlay | ours, overlays |
|---|---|---|---|
| `LinearLayout` root | `[0,275][1080,2337]` | `[0,0][1080,2062]` ✅ | ✅ |
| `TextView#count` "0" | `[42,317][63,368]` | `[42,42][63,93]` ✅ | ✅ |
| `Button#inc` | `[42,368][273,494]` (**231**) | `[42,93][309,219]` (267) ❌ | `[42,93][273,219]` ✅ |
| `Button#reset` | `[42,494][273,620]` | `[42,219][273,345]` ✅ | ✅ |

The variant step (E4H's `setAllCaps(false)` on all `TextView`s) now produces an
**identical view tree** — it is a no-op, which is the cleanest statement that the
divergence was theme resolution and is now gone. (Its PNG differs by 1 byte-class of
noise, SSIM also 0.99685.)

Everything else E4H measured is unchanged: measure/layout 1.59 ms, draw 464 µs cold /
86 µs warm, PNG 20 773 B, byte-identical output across runs, 1 reflective
`IWindowManager` proxy call still the only IPC.

## 4. The two blockers found (both harness-side; cause → fix → cost)

### 4.1 `idmap2` refuses to map anything without `--policy` (10 min)
First attempt (overlay APK anywhere, no flags) produced **no idmap** and 100+ lines of
`overlay '…' is not allowed to overlay resource 'string/config_helpPackageNameKey' in
target: overlay must be preinstalled, signed with the same signature as the target, …`,
then `error: no resources were overlaid -> failed to create idmap`. The message suggests
a path/signature problem; the actual cause is that the **policy** argument is missing —
`framework-res`'s resources are not declared `<overlayable>`, so the default policy
denies every entry. Measured matrix (all with the same two APKs):

| overlay APK location | flags | result |
|---|---|---|
| `/data/overlay/…` | *(none)* | ✗ no idmap |
| `/system/overlay/…` (preinstalled) | *(none)* | ✗ no idmap |
| `/system/overlay/…` | `--policy static` | ✗ `unknown policy: "static"` |
| `/system/overlay/…` | `--policy product` | ✓ 111 mappings |
| `/data/overlay/…` | `--policy product` | ✓ (identical idmap) |
| `/data/overlay/…` | `--ignore-overlayable` | ✓ (identical idmap) |

So the **location is irrelevant**; `--policy product` (the partition the overlay ships
on — `system` behaves the same, `static` is not a policy name) is what makes idmap2
emit the mapping. This is the flag OMS/idmap2d passes when it generates the same idmap
on first boot into `/data/resource-cache`. A side-effect worth knowing: idmap2 *warns*
about the one overlay entry it cannot map (`color/system_error`) and keeps going.

### 4.2 The overlay also rewrites the font families — and our harness's `sDefaults` was null (15 min)
With the overlay applied, inflate died:

```
InflateException: Error inflating class android.widget.TextView
  cause: NullPointerException: … field 'int android.graphics.Typeface.mStyle' on a null
         object reference in method 'Typeface create(Typeface, int)'
```

Cause chain, measured by resolving the overlaid strings through the same `Resources`:

```
string/config_bodyFontFamily        -> google-sans-text
string/config_bodyFontFamilyMedium  -> google-sans-text-medium
string/config_headlineFontFamily    -> google-sans
string/config_clockFontFamily       -> google-sans-clock
```

`TextAppearance.Material.Button` (and body text generally) now asks for
`google-sans-text`; that family does not exist in our root — and, verified against the
image, **not on the device either**: `product` has no `/fonts`, `system/fonts` has no
Google Sans, `system/etc/fonts.xml` declares only the 17 AOSP families. On a device the
lookup therefore returns `null` and `Typeface.create(null, style)` falls back to
`sDefaults[style]`, which zygote filled. In our harness `sDefaults[0..3]` were **null**
(`Typeface.<clinit>` ran with no font map, and E4H's seeding filled them from
`Typeface.DEFAULT`/`DEFAULT_BOLD`, which are themselves the permanent nulls ⇒ it wrote
nulls, then also nulled `sDefaultTypeface` *after* `setDefault()`).

Fix (10 lines, harness only): seed `sDefaults[0..3]` from the typeface that
`seedTypeface()` actually built (`Typeface.create(base, BOLD)` for index 1), and stop
nulling `sDefaultTypeface` after `setDefault()`. With that, `google-sans-text` resolves
to the default family exactly as on the device, and text metrics are unchanged
(`measureText("Increment") = 166 px`, the oracle's value) — i.e. the reference device
is *also* rendering Roboto, because the image ships no Google Sans.

**Product consequence for M-B/M-C:** font provisioning must set **`sDefaults[0..3]`**,
not only `sDefaultTypeface`/`sSystemFontMap`. Otherwise the first theme (or overlay)
that names a family the image lacks is an NPE in `Typeface.create`, not a silent
fallback. This is E4H §3.3's "do it the zygote way" item, sharpened: the zygote hands
over three things (font map, fallback map, **defaults**).

## 5. Cost, one place

| measurement | value |
|---|---|
| idmaps | 5 files, 152 / 284 / 460 / 976 / 1084 B (**2956 B** total) |
| idmap generation (one-off, offline) | 5 × ~0.37 s end-to-end under `guarded.sh` + bwrap; tool itself < 8 ms (below the bwrap floor of ~369 ms) |
| `ApkAssets.loadFromPath(target)` + 5 × `loadOverlayFromPath` | **1.19 ms** |
| same with 1 overlay | 0.82 ms |
| `AssetManager.Builder.build()` (7 ApkAssets) | 0.06 ms |
| whole process (ART init → frame → exit) | **261 ms** vs 266 ms control (noise) |
| peak RSS (`VmHWM`) | **251.8 MB** vs 251.4 MB control |
| frame | measure/layout 1.59 ms, draw 464 µs cold / 86 µs warm, PNG 20 773 B, md5 stable |
| determinism | `hov-view.png` md5 `2fc7c91db19d870ff18d66acc8f7f497` on repeated runs |

## 6. How this relates to ACOS-VM's overlay layer (`dbaf79b`, W198A)

ACOS-VM's `acos-framework-natives::res::AssetManager::apply_overlay` is the *same idea
implemented by hand*: read `android:targetPackage` from the overlay's binary manifest,
map each overlay entry onto the base by `(target package, type, key)` (what idmap2
computes), apply unconditionally, highest priority last. W198A measured its effect as
`inc 269 → 231 px`, i.e. **identical to what the stock path produces here (231)**.

The differences are exactly the ones W198A documented as deliberate limits:

| | ACOS-VM (`dbaf79b`) | stock path (this report) |
|---|---|---|
| mapping table | computed in-process from the two `.arsc`s | idmap2 (same tool the device uses) |
| what it can substitute | simple scalars only (`bool/int/color/dim/fraction`); `TYPE_STRING`, `@resid`, bags fall through to the base | everything the overlay defines, because the overlay is a real `ApkAssets` with its own string pool and package id |
| overlay APK visibility | must stay out of `sources` (Java string-block cache desync) | can be in the `ApkAssets` array (cookies line up) |
| runtime cost | ~0 (entries read at load) | +0.8 ms per overlay, +3 KB on disk |
| where the data comes from | `<boot>/overlays/*.apk` extracted at build time | `<boot>/overlays/*.apk` → `idmap2 create` at build time |

So the two agree on this app to the pixel, and the stock path removes the
"strings/refs/bags are skipped" caveat for free. Recommendation, cheapest first:
(a) keep ACOS-VM's layer (already shipped, 0 bytes on disk) and generate the overlays'
idmaps only if/when the product moves its resource engine to stock `libandroidfw`;
(b) if the product does move, generate idmaps at provisioning time with
`/system/bin/idmap2` (2956 B, sub-second) rather than implementing an idmap writer;
(c) either way, adopt §4.2's `sDefaults` seeding in the font provisioning step.

## 7. Not measured / next probes

- **Overlays that override non-scalars** (drawables, nine-patches, layouts, strings used
  by the layout). Stock path should handle them; the ACOS-VM layer provably cannot.
  Cheapest probe: `dialog-menu` or `canvas-shapes` with the same harness (0.5 d).
- **idmap freshness** — `ApkAssets.isUpToDate()` after replacing the overlay APK (the
  idmap carries both APKs' CRCs; a stale idmap should be rejected). Matters if overlays
  are re-generated in place.
- **Priority/conflict resolution** when two overlays override the same id (all five here
  are `priority=1`; no clash observed).
- **Dynamic (non-static) RROs** — those need `OverlayManager`/OMS state
  (`enable-exclusive`, user toggles) and are out of scope for a zygote-time loader.
- **Overlayable enforcement** — we generate idmaps with `--policy product`, i.e. we
  *decide* the policy offline. A product that ships overlays should record the policy
  with the idmap (idmap2 writes it into the header) rather than pass it at runtime.
- **APEX-hosted `framework-res`** and per-locale overlays (`framework-res__<locale>`) are
  not part of this image's static set; unmeasured.
