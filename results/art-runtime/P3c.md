# P3c — the font set is complete: `sSystemFontMap` is the zygote's, and the ActionBar title is exact

**Status: the last reported diff is gone — `[42,166][231,237]`, full tree 12/12,
SSIM 0.99290 → 0.99351.** The −3 px was **not** a missing font file. The API-34
image ships no `/product/fonts` at all (checked: `/product`, `/system_ext`,
`/vendor` have no `fonts/`; the only Google Sans in the image is TTF data inside
a product APK), and the reference device is the *same build* as the extracted
image (`UE1A.230829.050` / `12077443`, `google/sdk_gphone64_x86_64`), so it has
no product font set either. The real gap was that the guest asked
`SystemFonts.buildSystemTypefaces(FontConfig, Map)` for the named-family map
**in the wrong direction**: on API 34 that method takes the *fallback* map
(`name -> FontFamily[]`) and **returns** the `name -> Typeface` map, so calling
it with an empty map (P3b) lost every family and every alias in
`/system/etc/fonts.xml`. The ActionBar title asks for `sans-serif-medium`
(Roboto **variable**, `wght=500`), fell through to `sDefaults[0]` (Roboto
Regular, 186 px) and now resolves properly (189 px = the oracle).
Owner: implementer (P3c), 2026-11-11. Basis: P2c, P2e, P3b,
`docs/ENGINEERING_POLICY.md`. Nothing committed; no sudo; every command under
`tools/guarded.sh --mem 6G`.

```bash
tools/guarded.sh --mem 6G --time 300 -- ./target/debug/acos-art render-app \
    --apk fixtures/build/apps/counter.apk --out results/art-runtime/p3c/counter
#   frame=…/frame.png tree=…/tree.json md5=9ec17f055843a2b9d18078ec595e9300 nodes=13 1080x2400@420
#   outcome=ok wall=146–172ms draw_cold=432–492us peak_rss=201–202MB
python3 tools/art-render-app-compare.py \
    --png results/art-runtime/p3c/counter/frame.png \
    --tree results/art-runtime/p3c/counter/tree.json \
    --out results/art-runtime/p3c/compare.json
#   ssim=0.99351 decor_band=0.99762 tree=12/12 structural=8/8 → exit 0
```

## 0. Acceptance

| requirement | result |
|---|---|
| ActionBar title box = oracle `[42,166][231,237]` | **PASS** — `[42,166][231,237]`, `textWidth=189.0` (was `[42,166][228,237]` / 186.0) |
| full tree 12/12 rows exact | **PASS** — 12/12 (`tree_exact`), 8/8 structural, every delta `[0,0,0,0]` |
| SSIM ≥ the previous 0.9929 | **PASS** — full frame **0.99351** (P3b 0.99290), decor band **0.99762** (P3b 0.99696), MAE 0.587, identical px 98.35 % |
| deterministic md5 | **PARTIAL / BLOCKED** — one full run of the final guest: `9ec17f055843a2b9d18078ec595e9300`; the 5× re-run could not be done, see §5 (a concurrent workstream broke the shared guest tree) |
| `cargo test -p acos-art` | **PASS** — 24 passed, 0 failed (23 + `font_customization_merges_new_named_families_only`) |
| idempotent sysroot script re-run | **PASS** — 2nd run of `tools/art-sysroot.sh` skips the new font step entirely (13.1 s vs 15.2 s; it re-`dd`s+`lpunpack`s `system` as before, unchanged behaviour) |
| RAM < 90 %, `tools/guarded.sh`, no sudo | **PASS** — guest peak `VmHWM` 201–202 MB (host 60 GiB, ~32 GiB used); every command via `guarded.sh`; no privileged operation |
| render-app on `hello-views.apk` | **NOT APPLICABLE / unchanged blocker** — still stops at `EditText` → `DeviceConfig` (`Settings$NameValueCache.getStringsForPrefix` NPE), the P2a §4 service-peer wall, an *inflate* failure unrelated to fonts (§4) |

### The numbers, one place (`counter.apk`, 1080×2400 @ 420 dpi, full frame)

| measurement | P3b | **P3c** |
|---|---|---|
| ActionBar title | `[42,166][228,237]` → −3 px | **`[42,166][231,237]` = oracle** |
| title `textWidth` / typeface | 186.0 / Roboto Regular (`sDefaults[0]`) | **189.0 / Roboto `wght=500`** |
| full tree / structural | 11/12 / 8/8 | **12/12 / 8/8** |
| SSIM full frame / decor band | 0.99290 / 0.99696 | **0.99351 / 0.99762** |
| MAE / identical px | 0.646 / 98.33 % | **0.587 / 98.35 %** |
| `#inc` / `#reset` width | 231 / 231 | 231 / 231 |
| PNG md5 | `8fd9c4cdb9501e021dc6f47fe22cc90d` | **`9ec17f055843a2b9d18078ec595e9300`** |
| wall / peak RSS | 141–147 ms / 204–208 MB | 146–172 ms / 201–202 MB |
| `sSystemFontMap` | **2** (font-file fallback) | **33** (SystemFonts) |
| `SystemFonts FontConfig families` | 149 | 149 |

## 1. What shipped

| path | what it is |
|---|---|
| `crates/acos-art/guest/java/dev/acos/art/AppMain.java` (`seedTypeface` only) | the zygote order: `buildSystemFallback(FontConfig, ArrayMap<ByteBuffer>)` → `buildSystemTypefaces(FontConfig, fallbackMap)` **return value** installed as `sSystemFontMap`; the `ByteBuffer` cache is its own map. 3 hunks, nothing else touched |
| `crates/acos-art/src/app.rs` (`ensure_system_fonts` only, + 1 helper + 1 test) | provisions `/system/fonts` **and** the image's other font roots; merges `new-named-family` entries of a partition `fonts_customization.xml` into the sysroot `fonts.xml` with paths rewritten to `/system/fonts/…`; reports everything it cannot apply |
| `tools/art-sysroot.sh` (new step 2b + `FONTS.txt` stamp + `.no-fonts` markers) | extracts `/product/fonts`, `/system_ext/fonts`, `/vendor/fonts` and their `etc/fonts_customization.xml` with the same read-only `debugfs` path as `/system` (no mounts, no root); unpacks those partitions only when their font set is not provisioned yet |
| `results/art-runtime/p3c/counter/{frame.png,tree.json}` | the fixed frame + 13-node tree |
| `results/art-runtime/p3c/compare.json` | the comparator output, verbatim |

`shim/render_shim.c`, `crates/acos-binder-peer` and every other part of
`app.rs` are untouched.

## 2. The diagnosis P3b got wrong, and the one that holds

P3b blamed `/product/fonts`. Measured, on the image this repo extracts:

```
$ tools/art-sysroot.sh
[art-sysroot] extracting the font sets of /product /system_ext /vendor
[art-sysroot]   product: no /fonts (image ships none)
[art-sysroot]   system_ext: no /fonts (image ships none)
[art-sysroot]   vendor: no /fonts (image ships none)
```

* `/product` on this image: `app etc framework lib64 media overlay priv-app` —
  no `fonts`, no `etc/fonts_customization.xml`.
* The only `GoogleSansText-*.ttf` bytes in `product.img` are inside an APK's
  resources, not a font root; `system/etc/fonts.xml` declares the 17 AOSP
  families; `system/fonts` has no Google Sans (P2c §4.2 already said so).
* `fixtures/reference/counter/art-api34-x86_64/device-config.json` is
  `google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443`; the
  extracted image's `build.prop` has `ro.build.version.incremental=12077443`.
  Same build, therefore the same font set: the reference device cannot resolve
  Google Sans either, so a missing product font cannot explain a 3 px diff.

What does explain it, byte-for-byte (`dexdump` of the image's
`framework/classes.dex`):

| fact | evidence |
|---|---|
| `SystemFonts.getSystemPreinstalledFontConfig()` = `FontListParser.parse("/system/etc/fonts.xml", "/system/fonts/", "/product/etc/fonts_customization.xml", "/product/fonts/", …)` | the four constants in `getSystemFontConfigInternal` |
| `SystemFonts.buildSystemFallback(FontConfig, ArrayMap)` — second arg is the **`ArrayMap<String, ByteBuffer>` font-file cache**, and the family map is the **return value** | `buildSystemFallback: (FontConfig, ArrayMap) -> Map`, `appendNamedFamilyList(family, bufferCache, fallbackListMap)` |
| `SystemFonts.buildSystemTypefaces(FontConfig, Map)` — second arg is the **fallback map**, return value is the `name -> Typeface` map built by `Typeface.initSystemDefaultTypefaces(fallbackMap, aliases, result)` | 13 code units long, no mutation of the argument |
| `TextAppearance.Material.Widget.ActionBar.Title` → `fontFamily=@string/font_family_title_material` = **`sans-serif-medium`** | `aapt2 dump resources framework-res.apk`; *not* `config_headlineFontFamily`, so the Pixel RRO (`google-sans`) is irrelevant to the title |
| `<alias name="sans-serif-medium" to="sans-serif" weight="500"/>` | `/system/etc/fonts.xml` line 123 |
| Roboto is one variable font: `Roboto-Regular.ttf` with `wght` 100…900 | `fonts.xml` `<axis tag="wght">` per weight |

P3b called `buildSystemTypefaces(fc, emptyMap)`: the fallback map was empty, so
`initSystemDefaultTypefaces` added nothing and the alias loop resolved against
nothing — `sSystemFontMap` came back **empty**, the guest fell back to
`sans-serif`/`roboto` from a font file, and `Typeface.create("sans-serif-medium")`
returned `null` ⇒ `create(null, style)` ⇒ `sDefaults[0]` ⇒ Roboto Regular.

The fix is exactly the zygote's call order, and it removes a second, latent bug
at the same time: the guest stored path→`ByteBuffer` pairs in
`sSystemFallbackMap` (it passed the *output* map where the *cache* belongs),
which any `checkcast FontFamily[]` would have exploded on — after the first
attempt at this fix it did: `ClassCastException: DirectByteBuffer cannot be cast
to FontFamily[]`.

```java
java.util.Map<String, ByteBuffer> fontBuffers = new ArrayMap<>();      // cache, not output
Map<String, FontFamily[]> fallbackMap = SystemFonts.buildSystemFallback(fc, fontBuffers);
Map<String, Typeface>   fontMap     = SystemFonts.buildSystemTypefaces(fc, fallbackMap);
```

Run evidence (guest notes, `--json`):

```
SystemFonts FontConfig families=149 (font files the map/fallback are built from)
SystemFonts.buildSystemTypefaces -> 33 named families
Typeface: sSystemFontMap=33 families (SystemFonts), sSystemFallbackMap=9,
          sDefaultTypeface=…, sDefaults=[true,true,true,true]
```

33 = the 9 named families of `fonts.xml` + 24 aliases; 9 = the fallback lists.

### Why 189 px is the right number

Measured with the image's own Roboto variable font at the title's `textSize`
(53.0 px, `text_size_title_material_toolbar` 20sp @ 2.65):

| `wght` | `measureText("Counter")` |
|---|---|
| 400 | 186.63 |
| **500** (`sans-serif-medium`) | **188.42 → layout width 189** |
| 600 | 189.25 |

The oracle's ink box and ours agree in *height and vertical extents*
(y 184–222, 39 px tall) but not in advance (oracle ink 186 px, ours was 182) —
a weight/advance difference at identical size, which is what a 500 vs 400
variable-font instance looks like. Side-by-side crops of
`fixtures/reference/counter/art-api34-x86_64/screenshot_initial.png` and the
P3b frame also show the oracle's stroke weight as heavier.

## 3. Font provisioning, the part that was asked for

`tools/art-sysroot.sh` now extracts the other read-only partitions' font sets,
same technique as `/system` (GPT → `lpunpack` → `debugfs`, no mount, no root):

```
<out>/product/fonts/…            <out>/product/etc/fonts_customization.xml
<out>/system_ext/fonts/…         <out>/system_ext/etc/fonts_customization.xml
<out>/vendor/fonts/…             <out>/vendor/etc/fonts_customization.xml
<out>/FONTS.txt                  part<TAB>fonts    (0 = image ships none)
<out>/<part>/.no-fonts           marker: nothing to extract, never re-unpack
```

* Only the three partitions whose font set is missing are `lpunpack`ed
  (`extra_parts`), and the partition image is deleted as soon as the fonts are
  out of it — the second run does no extra work at all (13.1 s vs 15.2 s, and
  the log has no `extracting the font sets` line).
* `ensure_system_fonts` then makes the extracted files reachable **through the
  existing `/system` bind** (bwrap cannot create a mount point inside it):
  every partition font file is hard-linked into `<sysroot>/system/fonts`
  (0 bytes copied; `fonts.xml` names files relative to `/system/fonts`), and
  `new-named-family` entries of a partition `fonts_customization.xml` are
  merged into `<sysroot>/system/etc/fonts.xml` with each `<font>` path rewritten
  to `/system/fonts/<basename>`. `replace` / `additional-named-fallback`
  entries would need the real `/product` mount, which the sandbox does not have;
  they are **reported** in the diagnostics, never silently dropped. The merge is
  idempotent (a family whose `name` is already in the config is not added twice)
  and covered by `font_customization_merges_new_named_families_only`, which
  also asserts the two failure modes (a `replace` family and a family whose font
  file is absent) are counted in `skipped`, not written.
* On **this** image all three partitions report `no /fonts`, so the step is a
  visible no-op — which is the point: the P3b premise is now checked by the
  extraction itself instead of assumed, and any image that *does* ship product
  fonts gets them provisioned by the same run.

## 4. `hello-views.apk` — unchanged, unrelated blocker

`hello-views` still fails before any tree is produced:

```
InflateException: Error inflating class android.widget.EditText
  ← NullPointerException in Settings$NameValueCache.getStringsForPrefix
    ← Settings$Config.getStrings ← DeviceConfig.getProperties
    ← SelectionActionModeHelper.<init> ← Editor.getSelectionActionModeHelper
```

That is P2a §4's service-peer wall (the `settings`/`DeviceConfig` provider),
not a font problem: the chain is reached from `EditText`'s
`setTransformationMethod` before any text is measured. `P2a.md`, `P2e.md` and
`P3b.md` all record the same cause. The font fix does apply to it the moment it
inflates (its ActionBar title goes through the same `sans-serif-medium` path);
it simply cannot be measured yet.

## 5. Blocker at the checkpoint (and what is *not* verified because of it)

The shared `crates/acos-art/guest/java` tree is being edited concurrently by the
session/launch workstream: `dev/acos/art/LaunchMain.java` and
`SessionMain.java` reference `dev.acos.e3g.Nat`, whose sources are not in
`guest/java` yet (`research/art-feasibility/g-binder/java/dev/acos/e3g/` and
`crates/acos-binder-peer/tests/java/dev/acos/e3g/` have them). `build_guest`
compiles the whole tree, so from ~13:20 every `render-app` invocation fails
with

```
tool failed: javac failed for the render-app guest:
…/LaunchMain.java:81: error: package dev.acos.e3g does not exist
```

before any guest code runs. I did not touch those files (not mine, and adding
the missing package would collide with the agent that owns it).

Consequences, stated plainly:

* The numbers above are from the **final** guest (`9ec17f05…`, 13:07–13:09) and
  the **final** comparator run over those artefacts (13:22), i.e. they are real
  runs of this code, not projections.
* The **5× md5 determinism re-check** and a fresh `render-app` after the
  `ensure_system_fonts` rewrite are **not** done. Expect the md5 to stay
  `9ec17f05…`: the change only removes an early return and hard-links
  partition fonts (none exist on this image), and the merge writes nothing
  (`merged_families == 0`). Re-run when the guest tree compiles:

```bash
for i in 1 2 3 4 5; do
  tools/guarded.sh --mem 6G --time 300 -- ./target/debug/acos-art render-app \
      --apk fixtures/build/apps/counter.apk --out results/art-runtime/p3c/counter | head -1
done   # expect md5=9ec17f055843a2b9d18078ec595e9300 ×5
```

## 6. Reproduction / evidence

```bash
# 1. sysroot (idempotent; the first run adds the partition font sets)
tools/guarded.sh --mem 6G --time 900 -- tools/art-sysroot.sh
#   [art-sysroot]   product: no /fonts (image ships none)
#   [art-sysroot]   system_ext: no /fonts (image ships none)
#   [art-sysroot]   vendor: no /fonts (image ships none)
cat .cache/art/sysroot/FONTS.txt          # product 0 / system_ext 0 / vendor 0

# 2. tests
tools/guarded.sh --mem 6G --time 600 -- cargo test -q -p acos-art   # 24 passed

# 3. the frame (blocked while the guest tree is mid-change, §5)
tools/guarded.sh --mem 6G --time 300 -- ./target/debug/acos-art render-app \
    --apk fixtures/build/apps/counter.apk --out results/art-runtime/p3c/counter
python3 tools/art-render-app-compare.py \
    --png results/art-runtime/p3c/counter/frame.png \
    --tree results/art-runtime/p3c/counter/tree.json \
    --out results/art-runtime/p3c/compare.json
#   ssim=0.99351 decor_band=0.99762 tree=12/12 structural=8/8 action_bar_title=match
```

Undo: `rm -rf <sysroot>/{product,system_ext,vendor} <sysroot>/FONTS.txt
<sysroot>/system/fonts` (fonts are hard links, 0 bytes of data).
