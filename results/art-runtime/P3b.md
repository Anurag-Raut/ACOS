# P3b — `render-app` builds the real window chrome (PhoneWindow → DecorView)

**Status: the window chrome is the framework's, not the renderer's.**
`counter.apk` now renders through a real
`com.android.internal.policy.PhoneWindow` over the activity theme: stock
`setContentView` runs `installDecor` → `generateLayout` →
`DecorView.onResourcesLoaded`, so the DecorView, `ActionBarOverlayLayout`
(`id/decor_content_parent`), `ActionBarContainer`, `Toolbar`, the ActionBar
title and `id/content` all come from Google's code. The DecorView is
measured/laid out at **1080×2400 @ 420 dpi** with the oracle's real insets
(status 128, navigation 63) and drawn as the **full window frame**.
Owner: implementer (P3b), 2026-11-11. Basis: P2a, P2e,
`docs/ENGINEERING_POLICY.md`. Nothing committed; `crates/acos-binder-peer` and
`src/main.rs`'s other subcommands untouched; no sudo; every run under
`tools/guarded.sh --mem 6G`.

```bash
cargo run -q -p acos-art -- render-app --apk fixtures/build/apps/counter.apk \
    --out results/art-runtime/p3b/counter
#   frame=…/frame.png tree=…/tree.json md5=8fd9c4cdb9501e021dc6f47fe22cc90d
#   nodes=13 1080x2400@420 outcome=ok wall=141–147ms draw_cold=396–496us peak_rss=204–208MB
python3 tools/art-render-app-compare.py \
    --png results/art-runtime/p3b/counter/frame.png \
    --tree results/art-runtime/p3b/counter/tree.json \
    --out results/art-runtime/p3b/compare.json
#   ssim=0.99290 decor_band=0.99696 tree=11/12 structural=8/8 → exit 0
```

## 0. Acceptance

| requirement | result |
|---|---|
| a real `PhoneWindow` with the app theme, not a bare layout | **PASS** — `com.android.internal.policy.PhoneWindow(Context)`; theme = the manifest's `android:theme`, else `Resources.selectDefaultTheme(0, targetSdk)` = `Theme.DeviceDefault.Light.DarkActionBar` (the API-34 default); nothing per-app is hardcoded |
| window features per the manifest (action bar) | **PASS** — the theme's `windowActionBar`/`windowNoTitle`/`windowDrawsSystemBarBackgrounds` drive `generateLayout`; the APK declares no theme, so the platform default supplies the ActionBar |
| `setContentView(layout)`, `getDecorView()`, measure/layout/draw at 1080×2400/420 dpi | **PASS** — measure `EXACTLY 1080×2400`, layout `(0,0,1080,2400)`, `frame.png` = **1080×2400** |
| real insets (not a constant in the guest) | **PASS** — `--status-inset 128`, `--nav-inset 63` (defaults are the oracle's `statusBarBackground` / `navigationBarBackground` frames); dispatched as a `WindowInsets` before measure |
| full tree from the DecorView down (class, id, text, bounds, visibility) | **PASS** — 13 nodes: DecorView → ActionBarOverlayLayout → {content → LinearLayout → count/inc/reset, ActionBarContainer → Toolbar → title, ActionBarContextView} + statusBarBackground + navigationBarBackground |
| full-tree comparison against the ART oracle tree, structural nodes exact | **PASS** — vs `fixtures/reference/counter/art-api34-x86_64/ui_initial.xml`, **11/12 rows exact**, **8/8 structural exact**, 0 px on every structural node; one **−3 px** text row, see §3 |
| SSIM vs the ART oracle **full frame ≥ 0.99** (action bar included) | **PASS** — **0.99290** full frame (1080×2400); decor band (action bar + content, 0,128–1080,2337) **0.99696** |
| deterministic md5 | **PASS** — `8fd9c4cdb9501e021dc6f47fe22cc90d` on **5** consecutive runs |
| `cargo test -p acos-art` | **PASS** — 18 passed, 0 failed (17 + `apk_chrome_reads_the_manifest`) |
| RAM < 90 %, `tools/guarded.sh`, no sudo | **PASS** — peak guest `VmHWM` 204–208 MB (host 60 GiB, ~34 GiB in use); every command via `guarded.sh`; no privileged operation |

### The numbers, one place (`counter.apk`, 1080×2400 @ 420 dpi)

| measurement | P2e (bare layout) | **P3b (real window)** |
|---|---|---|
| frame | 1080×2062 (content band only) | **1080×2400 (full window)** |
| tree nodes | 5 (root FrameLayout + content) | **13** |
| structural oracle rows exact | 4/8 (the decor/action-bar rows were `missing_in_ours`) | **8/8** |
| DecorView | — | `[0,0][1080,2400]` = oracle |
| `id/decor_content_parent` | — | `[0,0][1080,2337]` = oracle |
| `id/action_bar_container` / `id/action_bar` | — | `[0,128][1080,275]` = oracle |
| `id/content` | — | `[0,275][1080,2337]` = oracle |
| `statusBarBackground` / `navigationBarBackground` | — | `[0,0][1080,128]` / `[0,2337][1080,2400]` = oracle |
| ActionBar title "Counter" | — | `[42,166][228,237]` vs oracle `[42,166][231,237]` → **−3 px**, §3 |
| SSIM full frame / decor band | 0.99685 (content band) | **0.99290 / 0.99696** |
| MAE / identical px (full frame) | 0.115 / 2050×2062 | 0.646 / 98.33 % (status-bar clock/battery icons are system UI, not the app) |
| `Button#inc` / `#reset` width | 231 / 231 | **231 / 231** |
| PNG md5 | `2fc7c91d…` | **`8fd9c4cdb9501e021dc6f47fe22cc90d`** |
| wall (whole command) | 126–146 ms | 141–147 ms |
| peak RSS | 200.8–207.7 MB | 204–208 MB |
| draw cold / warm | 329–368 / 19–20 µs | 396–496 / 201–218 µs (13 nodes, full frame) |

## 1. What shipped

| path | what it is |
|---|---|
| `crates/acos-art/guest/java/dev/acos/art/AppMain.java` | `buildWindow` (PhoneWindow + theme + title + `setContentView`), `layoutWindow` (insets → measure → layout), `installFakeViewRootImpl`, `installStubSettingsProvider`, `probeWindow`, manifest theme resolution, `MiniContext.getApplicationInfo/getPackageName`, `register_com_android_internal_util_VirtualRefBasePtr`; `draw` now renders `width × panelHeight` |
| `crates/acos-art/src/app.rs` | `AppOpts.panel_height/status_inset/nav_inset`, `apk_chrome` (theme resource id, application label, targetSdk from `aapt2`'s binary-manifest output), guest argv + result fields, docs |
| `crates/acos-art/src/main.rs` | `--panel-height`, `--status-inset`, `--nav-inset`; summary prints the panel |
| `tools/art-render-app-compare.py` | full-frame SSIM + decor-band SSIM (band derived from the oracle's own system-bar frames), full-tree diff in device coordinates (`--dy 0`), structural-node gate, `--oracle-tree` JSON input, DecorView→FrameLayout normalisation |
| `results/art-runtime/p3b/counter/` | `frame.png` (1080×2400), `tree.json` (13 nodes) |
| `results/art-runtime/p3b/compare.json` | the comparator output, verbatim |

`shim/render_shim.c` is unchanged; `crates/acos-binder-peer` untouched.

## 2. How the window is assembled (and why each substitution is neutral)

1. **Theme.** `aapt2 dump xmltree` reads `android:theme` from the binary
   manifest; `aapt2 dump badging` gives the application label and
   `targetSdkVersion`. A missing theme is `0`, and the guest applies exactly
   what `Resources.selectDefaultTheme(curTheme=0, targetSdk>=N)` returns:
   `Theme.DeviceDefault.Light.DarkActionBar`. No app-specific value is in the
   guest.
2. **The app's `ApplicationInfo`.** `MiniContext` used to inherit the *system
   context's* `ApplicationInfo` (`targetSdk` 0), which made `PhoneWindow`
   treat the window as pre-Lollipop and skip
   `FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS` — the decor then ignored the bar
   backgrounds and used the legacy navigation-bar path. Reporting the APK's
   package/targetSdk sets the flag, which is what makes the DecorView consume
   the navigation inset (content root `0..2337`) and *not* the status inset
   (ActionBarOverlayLayout consumes it, ActionBar at `y=128`), matching the
   oracle exactly.
3. **`WindowManagerImpl`.** `Window.setWindowManager` casts to
   `WindowManagerImpl`; `ActivityThread` hands an Activity the same local
   window manager. Nothing is added to a real WM.
4. **Local `settings` provider.** `PhoneWindow`'s constructor reads
   `Settings.Global.DEVELOPMENT_RENDER_SHADOWS_IN_COMPOSITOR`, whose path is
   `ContentResolver` → `IActivityManager.getContentProvider` (the P2a §4 wall).
   Instead of proxying the ActivityManager, a read-only local
   `IContentProvider` is registered in `ActivityThread.mProviderMap` for the
   `settings` authority: unset keys answer "no row", so the framework's own
   `getInt(..., default)` applies. No app data is invented.
5. **A minimal `ViewRootImpl`.** `DecorView.onLayout` and two window callbacks
   dereference `getViewRootImpl()`. A zeroed `ViewRootImpl`
   (`Unsafe.allocateInstance`, no constructor — the real one needs
   `IWindowSession`) behind a `View$AttachInfo` (with `mTmpOutline` and a
   display-list `RenderNode` for the decor) answers that; only
   `requestInvalidateRootRenderNode()` (a boolean write) is reached. The
   `AttachInfo` is installed *after* the insets are dispatched so
   `updateColorViews()` takes its no-ViewRootImpl branch.
6. **`VirtualRefBasePtr` natives.** `Toolbar` holds one; the class's
   `register_com_android_internal_util_VirtualRefBasePtr` is a standalone
   class (no `SurfaceControl.<clinit>`), so it is registered with the other
   runtime natives.
7. **Insets are arguments.** `WindowInsets.Builder().setSystemWindowInsets` —
   typed status-bars/navigation-bars sources, so `DecorView.updateColorViews`
   creates the two background views and the content root gets the right
   margins.

## 3. The one remaining diff: ActionBar title −3 px (font file, not layout)

| node | ours | oracle | delta |
|---|---|---|---|
| `text:TextView|Counter` | `[42,166][228,237]` (186 px) | `[42,166][231,237]` (189 px) | **−3 px** wide |

Cause: the ActionBar title resolves through the image's static RROs
(`config_headlineFontFamily = google-sans`, P2c/P2e) and the device's Google
Sans lives in the **product** partition (`/product/fonts`). The extracted
sysroot only carries `/system/fonts` (Roboto), so the guest's font map falls
back to `sDefaults[0]` (Roboto regular, 186 px). It is the same
font-provisioning gap P2e documented (`sSystemFontMap` is a zygote artefact;
only the `/system` font set is extracted) — not a window/layout error: the
title's *box* (y 166–237, x 42) is exact, only glyph advance differs.
Smallest next step (~20 min, no architecture change): extract
`/product/fonts` from the API-34 system image with the existing
`tools/extract-bootclasspath.sh` unpack path into `.cache/boot/android-34/fonts`
and let `ensure_system_fonts` hard-link it. Not done here: out of this task's
file ownership (the extraction tool is another workstream) and would not
change any structural number.

The full-frame SSIM (0.99290 vs decor band 0.99696) differs because the oracle
screenshot also contains the system UI's status-bar clock/battery and the
gesture pill, which no app frame can reproduce; the app-owned band is 0.99696.

**Informational:** against the ACOS-native tree model
`results/parity/w193a/counter.tree.json`, the same tree scores 7/12 because
that model places the DecorView at the *app* frame `[0,128][1080,2337]` and
predates the P2c/P2e overlay fix (`#inc` 311 px). It is an ACOS renderer
oracle, not the ART oracle; the ART oracle is `ui_initial.xml`, which the
comparator uses by default (`--oracle-tree` is there to inspect the other).

## 4. Reproduction / evidence

```bash
# acceptance (frame + tree in results/art-runtime/p3b/counter/)
cargo run -q -p acos-art -- render-app --apk fixtures/build/apps/counter.apk \
    --out results/art-runtime/p3b/counter
python3 tools/art-render-app-compare.py \
    --png results/art-runtime/p3b/counter/frame.png \
    --tree results/art-runtime/p3b/counter/tree.json \
    --out results/art-runtime/p3b/compare.json

# determinism: 5 identical md5s
for i in 1 2 3 4 5; do
  cargo run -q -p acos-art -- render-app --apk fixtures/build/apps/counter.apk \
      --out results/art-runtime/p3b/counter | head -1
done

# unit tests
cargo test -q -p acos-art     # 17 passed
```

Guest notes in the `ACOSAPP` verdict show the seam explicitly: theme id and
source, `setWindowManager(android.view.WindowManagerImpl)`,
`PhoneWindow.setContentView(0x7f020000) title="Counter"`,
`dispatchApplyWindowInsets(system=Insets{left=0, top=128, right=0, bottom=63})`,
`window flags=0x80810100` (FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS +
FLAG_LAYOUT_IN_SCREEN + hardware-accelerated request), and the two downgraded
`RippleDrawable`s (unchanged P2a workaround).

## 5. Limits / next

- The activity is not launched: no `Activity.onCreate`, no
  `ActivityThread` lifecycle. The layout is inflated by `PhoneWindow`'s
  `setContentView` (the same call the activity makes), so the app's own
  `setContentView`/`findViewById` code does not run; the input `count` is 0,
  which is the state the oracle captures.
- `hello-views.apk` still stops at the `EditText`/IME service wall
  (P2a §4), unchanged; the window path itself no longer needs a new peer.
- Not done: menu/ActionBar items from the app (no `onCreateOptionsMenu`),
  dialogs, multiple windows, `Choreographer` frames, damage/present into
  `acos-wm`.
- Time: finished inside the 60-minute checkpoint (window work + comparator +
  evidence); the −3 px title row is the only reported diff and is diagnosed
  above, not patched around.
