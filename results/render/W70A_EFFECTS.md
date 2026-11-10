# W70-A — renderer effects: colour glyphs, elevation shadows, blur sigma, bitmap shaders

**Worktree:** clean git worktree of `f7f7d93` (`.cache/w70a`), `CARGO_TARGET_DIR=.cache/target-w70a`,
`CARGO_PROFILE_DEV_DEBUG=0`, all builds via `tools/guarded.sh`. Patch applied back to
`main` with `git apply --check` (clean). Nothing committed.

## What changed (all in `crates/acos-render`, plus one test)

| File | Change |
|---|---|
| `src/text/freetype.rs` | `FT_LOAD_COLOR` + BGRA handling; `GlyphMask.color` (premultiplied RGBA8); fixed-strike selection + bilinear resampling for bitmap-only CBDT/CBLC faces; `scale_rgba`. |
| `src/text/fonts.rs` | Parse `fonts.xml` `ignore="true"` into `Family.ignored`; ignored families are loaded but never selected by name/fallback (matches Android). |
| `src/canvas.rs` | `blit_color` (premultiplied RGBA colour-glyph composite); mask expand/filter use the Skia sigma; variance-matched 3-box Gaussian (`r = (sqrt(1+4σ²)-1)/2`). |
| `src/paint.rs` | `radius_to_sigma = radius * 0.57735 + 0.5` (`SkBlurMask::ConvertRadiusToSigma`). |
| `src/context.rs` | Two-lobe elevation shadow (soft ambient + offset spot); text blit dispatches to `blit_color` for colour glyphs. |
| `src/scene.rs` | `BitmapShader` scene variant (`{"type":"bitmap","path":…,"tile":…}`) + relative-path resolution. |
| `tests/material_effects.rs` | New deterministic tests: bitmap tiling, two-lobe shadow, blur-sigma spread. |

## Measured results

```
cargo test -p acos-render           -> lib 77, contract 18, corpus 1, golden 2,
                                       hello_views 1, present_damage 4,
                                       reference_diff 2, text_coverage 1,
                                       text_metrics 1, variable_axes 2,
                                       material_effects 3   (all ok, 0 failed)
cargo clippy -p acos-render --all-targets -- -D warnings -> 0 warnings
cargo test -p acos-vm --test gfx_canvas --test gfx_natives -> 5 passed
tools/diff-dexprogs.sh --corelib real -> 27 pass / 0 fail / 0 unsupported
```

The existing 5-scene ART corpus (`tests/corpus.rs`, `fixtures/reference/*/art-api34-x86_64`)
still passes unchanged: SSIM 0.989–0.995, MAE 0.39–0.70 (the two-lobe shadow does not
alter any elevation-0 corpus node).

## Honest status of the requested gaps

| # | Gap | Status |
|---|---|---|
| 1 | Emoji / colour fonts | **Rasteriser path done for BGRA colour glyphs.** FreeType is asked with `FT_LOAD_COLOR`; COLRv0/CPAL, CBDT/CBLC strikes and sbix all produce a premultiplied BGRA bitmap that is blitted with `blit_color`. Bitmap-only CBDT faces get nearest-strike selection + bilinear resampling to the text size (unit test on `NotoColorEmojiLegacy.ttf`). **Not yet colour:** Android 14's default `NotoColorEmoji.ttf` is **COLRv1**, which FreeType's automatic `FT_Render_Glyph` path does not rasterise (it only walks COLRv0 layers); a COLRv1 paint walker (`FT_Get_Color_Glyph_Paint`/`FT_PaintGlyph`) is required and is **not implemented**. Today a COLRv1 emoji falls back to the base outline, not a colour bitmap. This is a structured gap, not a silent wrong result. |
| 2 | Material elevation shadows | **Done (two-lobe).** `draw_node_shadow` now draws a broad ambient lobe plus a spot lobe displaced by the elevation, both through the radius→sigma blur, approximating `SkShadowUtils` with the vertical-light default. Covered by `material_effects::elevation_paints_ambient_and_spot_shadow_lobes`. No ART reference for an elevated Material card was captured (see below). |
| 3 | RenderEffect blur + MaskFilter sigma | **MaskFilter sigma semantics done** (`sigma = radius*0.57735+0.5`, variance-matched box passes; unit test). **`RenderNode.nSetRenderEffect` still returns a structured `UNSUPPORTED_JAVA_API`** — no node-level offscreen render-effect model was added. |
| 4 | Bitmap shaders + tile modes | **Scene format done** (`bitmap` shader variant, tile modes, relative path resolution) and exercised by `bitmap_shader_repeats_the_bitmap`. `ComposeShader` is not modelled. Native shader construction is still not wired (no gradient/bitmap shader native exists to wire to). |
| 5 | Material drawables | **Not implemented.** `RippleDrawable`, `StateListDrawable`, `GradientDrawable`, `LayerDrawable`, nine-patch stretch regions remain unexercised; `nDrawNinePatch` still records "exact chunk stretching" as unsupported. X has no framework drawable natives yet. |

## ART references — what was and was not captured

The API-34 oracle emulator **was booted** during this task
(`tools/oracle/emulator.sh start`, headless, 2048 MB guest, booted in ~14 s from the
AVD snapshot, host `MemAvailable` ≈ 41 GiB) and shut down afterwards. No new fixture
APK was authored within the time box, so **no new ART capture was produced for the
effects above**. The per-scene reference diffs therefore remain the five W66-A scenes.
Adding a `material-effects` fixture app (one `View` drawing an elevated rounded card, a
`BlurMaskFilter` rect, a `BitmapShader` rect and an emoji run) and capturing it on the
same emulator is the concrete next step; the renderer-side scene ops it needs now exist.

## Reproduce

```sh
ACOS_FONT_DIR=$PWD/.cache/boot/android-34/fonts \
  cargo test -p acos-render --test material_effects
ACOS_FONT_DIR=$PWD/.cache/boot/android-34/fonts \
  cargo test -p acos-render
cargo clippy -p acos-render --all-targets -- -D warnings
```
