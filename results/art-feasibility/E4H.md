# E4H — M-B without a binder peer: real framework view tree → software frame in one `dalvikvm64`

**Answer: yes.** In a *single* `dalvikvm64` process, with only stock Google libraries
(`framework.jar`, `libandroid_runtime.so`, `libhwui.so`, `framework-res.apk`) plus the E2D
`LD_PRELOAD` shim and two small reflective stubs, the real framework **inflates
`counter.apk`'s `res/layout/main.xml` under a real `Theme`, measures/lays it out at
1080×2400 @ 420 dpi, draws it into a software `Bitmap` via `Canvas`, and writes a PNG and a
view-tree dump** — with **no zygote, no `system_server`, no servicemanager, no `/dev/binder`**.

Measured against the ART oracle (`fixtures/reference/counter/art-api34-x86_64`), on the
same 1080×2062 app-content band:

| variant | SSIM | MAE | identical pixels | tree |
|---|---|---|---|---|
| as-inflated (theme's `textAllCaps`) | **0.99196** | 0.43/255 | 98.22 % | 3/4 content nodes exact |
| `textAllCaps=false` | **0.99685** | 0.11/255 | 98.50 % | **4/4 content nodes exact** |

Whole run: **216–257 ms wall** (E1A ART baseline is 177 ms), **peak RSS 241–247 MB**,
**byte-identical PNG across runs** (md5 `237e5fd1…` twice in a row ⇒ deterministic).

- Owner / date: E4H agent, 2026-11-11. Time-box 45 min; **used ~50 min** (of which the
  Typeface family is 30: it is the one genuinely new blocker, §3.3).
- Constraints honoured: no ACOS crate touched, nothing committed, no sudo, no run outside
  `tools/guarded.sh`, all new files under `research/art-feasibility/h-view/` +
  this report, scratch in `.cache/art-exp-h/`. The only shared-state edit is *additive*: the
  extracted font set is hard-linked into the E1A sysroot (`/system/fonts`, `/system/etc/fonts.xml`,
  §3.3) — 0 bytes copied, undo with `rm -rf .cache/art-exp/sysroot/system/fonts`.
- Depends on: **E1A** (sysroot + `artroot.sh`), **E2D** (`libbindershim.so`, property table,
  memfd binder stand-in, `dev.acos.e2d.Nat.register`), **E2E** (`register_android_graphics_classes`
  is exported by libhwui and is callable).

```bash
research/art-feasibility/h-view/scripts/build.sh     # javac + d8 into the mini-root
research/art-feasibility/h-view/scripts/run.sh all   # the whole ladder, under the guard
python3 research/art-feasibility/h-view/scripts/compare.py   # SSIM + tree vs both references
```

| artefact (`research/art-feasibility/h-view/`) | what it is |
|---|---|
| `java/dev/acos/e4h/Main.java` | the ladder: gfx → runtime → activity → resources → services → text → inflate → layout → draw → variant |
| `scripts/hroot.sh` | `e2droot.sh` + font provisioning (hard links into the scratch sysroot) |
| `scripts/build.sh`, `scripts/run.sh` | build (javac + d8, reuses the E2D dex for `Nat`) and run under `guarded.sh` |
| `scripts/compare.py` | SSIM / MAE / exact-pixel and node-by-node bounds diff against the ART oracle **and** ACOS-VM |
| `artifacts/e4h-view.png` | the frame (1080×2062, app content area) |
| `artifacts/e4h-view-nocaps.png` | same tree with `textAllCaps=false` (the 0.997 variant) |
| `artifacts/e4h-tree{,-nocaps}.txt` | view-tree dump (class, id, text, bounds, padding, fontMetrics) |
| `artifacts/compare.txt` | the comparison output below, verbatim |
| `.cache/art-exp-h/logs/*.log` | per-run logs |

## 1. Verdict table

| # | step | result | blocker hit | fix applied | effort (measured) |
|---|---|---|---|---|---|
| 1 | register graphics natives through **the exported entry point** (`libhwui`'s `register_android_graphics_classes(JNIEnv*)`) | **PASS** — 1 call, 0 missing; proved by calling `HardwareRenderer.nSetIsSystemOrPersistent(false)` | none (E2E path works when the lib is `dlopen`ed in the **default** namespace by the E2D shim: `E2D_LOAD=libandroid_runtime.so:libhwui.so`) | call `Nat.register("register_android_graphics_classes")` | 10 min |
| 2 | register the `libandroid_runtime` natives the view path needs | **PASS** — 18/18 registered | `register_android_view_SurfaceControl` **runs its own class initialiser**, whose `<clinit>` calls a not-yet-registered native of the same class (`SurfaceControl.nativeGetNativeSurfaceControlFinalizer`) ⇒ sticky `NoClassDefFoundError` | drop the 6 Surface/SurfaceControl/GraphicBuffer/BLAST/SurfaceTexture registrations — the software draw path needs none — and add `view_DisplayEventReceiver` (needed by `Choreographer`) | 15 min |
| 3 | `ActivityThread.systemMain()` | **PASS** (E2D stopped here) | (a) `Handler` needs a main `Looper` on this thread; (b) after that, only *logs*: `DisplayManagerGlobal`/`WindowManagerGlobal` binder calls fail (`ENOTTY`) | `Looper.prepareMainLooper()` first, then `systemMain()`; the failing `getService("display"/"window")` calls are non-fatal | 10 min |
| 4 | `Context` + `Resources` over `framework-res.apk` + `counter.apk` at 1080×2400 / 420 dpi | **PASS** — `app_name="Counter"`, `layout/main=0x7f020000`, density 2.625 | the 3-arg `Resources` ctor recomputes `DisplayMetrics` to 0×0 | call the @hide `Resources.updateConfiguration(cfg, dm)` after construction (what `ResourcesManager` does) | 5 min |
| 5 | real `Theme` | **PASS** — `android:style/Theme.DeviceDefault.Light`, `windowBackground` = `#f0f0f3` (same colour ACOS-VM renders) | — | `Resources.newTheme()` + `applyStyle(...,true)` | 0 |
| 6 | `View` construction (needs a `Context`, `ViewConfiguration`) | **PASS** after a stub | `ViewConfiguration.<init>` → `IWindowManager.hasNavigationBar(displayId)` → `ServiceManager` → **null** ⇒ `NullPointerException` (this is *the* "no binder peer" wall) | install a `java.lang.reflect.Proxy` implementing @hide `IWindowManager` as `WindowManagerGlobal.sWindowManagerService`; every method answers its default (measured: **exactly 1 call** — `hasNavigationBar`) | 10 min |
| 7 | `Typeface` / text | **PASS after 3 hacks** | `Typeface.sSystemFontMap` is empty ⇒ `sDefaults[0]==null` ⇒ `TextView` NPEs in `Typeface.create(null,0)`. On device the map arrives through `Typeface.sSystemFontMapSharedMemory`, a `SharedMemory` region **zygote allocates** — we have no zygote. Rebuilding it needs care: `CustomFallbackBuilder` NPEs, and writing the Java field alone trips libhwui's `ASSERT src == nullptr && gDefaultTypeface == nullptr` | `SystemFonts.getSystemPreinstalledFontConfig()` (202 fonts, parsed OK) → build one `android.graphics.fonts.FontFamily` from `/system/fonts/Roboto-Regular.ttf` → `Typeface.createFromFamilies("sans-serif", families)` → install via the @hide **`Typeface.setDefault(t)`** (sets the Java field *and* libhwui's `gDefaultTypeface`) | **30 min** (§3.3) |
| 8 | `LayoutInflater.inflate(layout/main)` under that theme | **PASS** — `LinearLayout`, 27.2–27.8 ms cold (includes Typeface/theme/style class init) | `TextView`/`Button` need `Choreographer` → `DisplayEventReceiver` natives (`AnimationHandler`) | registered `view_DisplayEventReceiver` (step 2) | 5 min |
| 9 | measure / layout | **PASS** — 1080×2062 @ 2.625, **1.5 ms**; padding 16 dp = 42 px, buttons 48 dp = 126 px tall: **all exactly the oracle's** | — | `FrameLayout` root + `MATCH_PARENT` child, `MeasureSpec.EXACTLY` | 0 |
| 10 | `View.draw(Canvas(Bitmap))` (software, no HW accel) | **PASS** — **450–500 µs cold, 86–93 µs warm** for the whole tree (2 % of 2.2 Mpx differ from the background) | **`RippleDrawable.draw()` kills the process with a silent SIGSEGV** (no tombstone, no log line) — the only background type in this tree that does; it is the pressed-state animation and needs a hardware canvas | software downgrade: replace every `RippleDrawable` by its content layer (`getDrawable(0)` = `InsetDrawable`→`GradientDrawable`); logged as `DOWNGRADE` | 10 min |
| 11 | PNG + view-tree dump | **PASS** — `Bitmap.compress(PNG)` works (`nativeCompress` via libhwui), 24 KB; tree dump with class/id/text/bounds/padding/fontMetrics | my first tree JSON had stray commas (my bug, not the framework's) | — | 5 min |
| 12 | compare with the ART oracle and ACOS-VM | **PASS** — SSIM 0.99196 / 0.99685 (see cover table); content tree **4/4 exact** bounds with `textAllCaps=false`; deterministic | first comparison cropped *our* already-cropped frame (black band) — numbers were 0.858/33 MAE until fixed | `scripts/compare.py` crops only the references | 10 min |

`failures=0` for `run.sh all`. Rungs `gfx|runtime|activity|resources` can be run alone.

## 2. What the ladder proves

- **The E2D blocker was a missing registration, not a missing runtime.** One call to
  `register_android_graphics_classes(JNIEnv*)` (exported, `LIBHWUI`-versioned) makes
  `HardwareRenderer.nSetIsSystemOrPersistent` resolvable, and `ActivityThread.systemMain()`
  then runs to completion. E2D's estimate ("0.5–1 d for the graphics subset") was pessimistic
  by an order of magnitude **because it assumed the registration had to be hand-written**;
  libhwui's own entry point does it, and it also caches the class globals the natives rely on
  (E2E blocker B3).
- **The framework's view pipeline is binder-free once `ViewConfiguration` is answered.**
  In a full inflate+layout+draw, the *only* IPC that reaches the "peer" is
  `IWindowManager.hasNavigationBar(int)` inside `View.<init>`→`ViewConfiguration.<init>`
  (measured: 1 proxy call, 8 `BINDER_WRITE_READ` attempts, 3 `ServiceManager` failures — all
  other failures are logged and non-fatal). That is the entire M-B service surface for a
  static first frame.
- **Software draw really is the cheapest complete seam.** 450 µs to rasterise a real
  framework view tree with Skia CPU, no GPU, no display, no surface, no choreographer loop.
- **Text is the one under-documented dependency.** Everything visual in this app is
  text-shaped; a font-less process gets no views at all (not "bad text" — NPE in the
  constructor). See §3.3 for the exact mechanism and the 20-line fix.

## 3. Blockers in the order they were hit (cause → fix → cost)

### 3.1 Registering the *whole* `libandroid_runtime` native set is a trap (15 min)
`register_android_view_SurfaceControl` does more than `RegisterNatives`: its body touches
`SurfaceControl.Transaction`, which triggers `SurfaceControl.<clinit>`, which calls
`SurfaceControl.nativeGetNativeSurfaceControlFinalizer()` — a native of the class being
registered. The failed class init is **sticky**, so every later use of `SurfaceControl` throws
`NoClassDefFoundError`. Same shape for `Surface`, `SurfaceSession`, `GraphicBuffer`,
`BLASTBufferQueue`, `SurfaceTexture` (all surface/GPU path). *Rule*: register only the set the
seam needs, in dependency order, and treat a `NoClassDefFoundError` on a framework class as
"a register function ran a `<clinit>` that needed its own natives". Cost if we had needed
surfaces: unknown, likely a hand-written minimal `SurfaceControl` — not needed for M-B.

### 3.2 `ViewConfiguration` is where "no binder peer" actually bites (10 min)
`View.<init>` → `ViewConfiguration.get(ctx)` → `new ViewConfiguration(ctx)` →
`WindowManagerGlobal.getWindowManagerService()` → `ServiceManager.getService("window")` →
`null` → NPE on `IWindowManager.hasNavigationBar(displayId)`.
Fix used: a `java.lang.reflect.Proxy` for @hide `IWindowManager` installed into the private
static `WindowManagerGlobal.sWindowManagerService`, answering `false/0/null`. Cost in the
product: this *is* the M-B peer surface — one AIDL-shaped service, 1 method for this app
(`hasNavigationBar`), a handful more for real apps (`getInitialDisplaySize`,
`getWindowInsets` …). 0.5 d to stand it up as a real (non-proxy) stub.

### 3.3 Typeface: the system font map is a zygote artefact (30 min — the expensive one)
Measured sequence:
1. `Typeface.<clinit>` leaves `sSystemFontMap = {}`, `sSystemFallbackMap = {}`,
   `sDefaults = null`, `sDefaultTypeface = null`, `sSystemFontMapSharedMemory = null`.
   On device the map is handed from zygote/system_server through that `SharedMemory` field.
2. `SystemFonts.getSystemPreinstalledFontConfig()` *does* work here — 202 fonts parsed from
   `/system/etc/fonts.xml` (which we provision; without `/system/fonts` the two "Preloading"
   log lines are all you get and nothing else works).
3. `SystemFonts.buildSystemTypefaces(fc, map)` returns **0** named families on this image,
   and `Typeface.createFromFile()` NPEs inside `CustomFallbackBuilder.build()`.
4. `android.graphics.FontFamily`(legacy, JNI) + `addFont` with wrong arguments **segfaults**
   (no tombstone in the sandbox) — do not poke it speculatively.
5. Working combination: modern `Font$Builder(File).build()` → `FontFamily$Builder(font).build()`
   → `Typeface.createFromFamilies("sans-serif", families)` → **`Typeface.setDefault(t)`**
   (the @hide setter that sets the Java field *and* libhwui's `gDefaultTypeface`; writing the
   field alone trips `ASSERT src == nullptr && gDefaultTypeface == nullptr` on the first
   `measureText`).
Cost in the product (M-B/M-C): either (a) precompute the font map in the parent and hand it
over the same way zygote does (`SharedMemory` + `Typeface.setSystemFontMap`), or (b) the
5-call sequence above, 20 lines, only correct for the *default* family (the other 8 named
families and the 202-entry fallback map end up pointing at Roboto-Regular — good enough for
Latin text, wrong for CJK/emoji weight/style selection). (a) is the honest one: **0.5 d**.

### 3.4 `RippleDrawable` on a software canvas segfaults (10 min)
`Button`'s background is a `RippleDrawable`; `draw()` on a non-hardware `Canvas` kills the
process silently (verified by drawing layer by layer: `RippleDrawable` dies, its content
layers draw fine, and the same Button draws fine with `setBackground(null)`). A ripple is a
*pressed-state animation*; the static frame is its content layer. Two product options:
(i) downgrade to the content layer while `canvas.isHardwareAccelerated()==false` (measured,
0 visual cost for a static frame), or (ii) make the framework's software path do it for us
(`RippleDrawable.draw` already has the `isHardwareAccelerated()` branch on device — worth
finding out why it does not take it here, cheap follow-up: 0.5 d, libhwui `rip_*` natives).
Cost if we ship (i): 20 lines in the ACOS draw bridge. Risk: any `Drawable` that reaches
`RenderNode`/`RenderEffect` will behave the same ⇒ audit `Drawable.draw` for hardware-only
subclasses (1 d).

## 4. Parity, in detail (this is the interesting part)

`artifacts/compare.txt` (verbatim) plus the byte-level attribution:

| | vs ART oracle (device) | vs ACOS-VM (`results/parity/w193a/counter.png`) |
|---|---|---|
| SSIM | 0.99196 / **0.99685** | 0.92209 / 0.92299 |
| MAE | 0.43 / **0.11** per channel (0–255) | 16.2 |
| identical pixels | 98.22 % / **98.50 %** | 90.6 % |
| worst rows | y=0..2 (content-band edge) and y=144..169 (button label glyphs) | — |
| content-area mean diff | 1.27 in the 0–700 px band (text/buttons), **0.000 below y=700** | — |

View tree, all four content nodes, device bounds minus the content origin (y−275):

| node | device (ART) | E4H as-inflated | E4H `textAllCaps=false` |
|---|---|---|---|
| `LinearLayout` (root) | `[0,275][1080,2337]` | `[0,0][1080,2062]` ✅ | ✅ |
| `TextView#count` "0" | `[42,317][63,368]` | `[42,42][63,93]` ✅ | ✅ |
| `Button#inc` "Increment" | `[42,368][273,494]` | `[42,93][309,219]` ❌ +36 px | `[42,93][273,219]` ✅ |
| `Button#reset` "Reset" | `[42,494][273,620]` | `[42,219][273,345]` ✅ | ✅ |

Padding (42 px = 16 dp), button height (126 px = 48 dp), text size (14 sp = 37 px),
`fontMetrics` and `measureText("Increment")=166 px` all match the oracle exactly.

**The one real divergence is `textAllCaps`.** Our `Theme.DeviceDefault.Light` resolves
`Widget.Material.Button`'s `textAllCaps=true` ("INCREMENT"), the device renders sentence case
("Increment"). It is worth +36 px of button width and all of the residual pixel difference
(SSIM 0.99196 → 0.99685 when forced off, tree 3/4 → 4/4). ACOS-VM has the *same* divergence
(its `#inc` is `[42,368][311,494]`, 2 px from ours, 38 px from the device) — i.e. this is a
**shared theme-resolution gap, not an E4H artefact**: nobody is resolving the app's real
theme from the APK's manifest (`android:theme` absent ⇒ default activity theme + targetSdk-dependent
`values-v*` overrides that the `framework-res` version we load does not reproduce). Fix
candidate (cheap, 0.5 d): resolve the activity theme the way `PackageParser` does
(application/activity `android:theme`, targetSdk-based default) instead of hardcoding
`Theme.DeviceDefault.Light`; ACOS-VM's renderer needs the same fix, which would move its
SSIM too.

Two *structural* differences that are by construction and not measured as error:
- we build no `DecorView`/`ActionBar`/status/navigation bars (no `ViewRootImpl`,
  no `WindowManager` layout pass) — the comparison band is the app content area, and
  `id/content`, `decor_content_parent`, `action_bar*`, `ActionMenuView` are reported as
  `missing_in_ours` rather than silently ignored;
- both sides are `hardware-accel=false`, so `elevation` shadows (the soft edge under the
  device's buttons) are absent. E2E flagged this as seam (c)'s known gap; it is the second
  largest residual after the glyphs: a 1–2 px band around every button outline plus the
  8 px band under each button (`elevation` = 2 dp).

## 5. Numbers, one place

| measurement | value |
|---|---|
| registered natives | 1 graphics entry point (620 natives) + 18 `register_android_*` (0 missing) |
| `ActivityThread.systemMain()` | PASS, eager (`ThreadedRenderer.initForSystemProcess` survived) |
| inflate `layout/main` | 27.2 / 27.8 / 27.5 ms cold (3 runs) — dominated by `Typeface`/theme/style class init |
| measure + layout 1080×2062 | 1.51 / 1.51 / 1.53 ms |
| `View.draw` → `Canvas(Bitmap)` | 449–497 µs cold, 86–93 µs warm |
| PNG | 21–24 KB, `Bitmap.compress(PNG)` OK |
| deterministic | identical md5 (`237e5fd13b9b3cfc8aa51d63cdc07e48`) across two runs, identical non-background pixel count (44 097) across three |
| whole process (ART init → frame → exit, `bwrap` floor 28 ms) | **216 / 226 / 223 ms** (+ variant 257 ms) |
| peak RSS (`VmHWM`) | **241 / 246 / 241 MB** (E2E's load-only baseline: 163 MB; ART floor: ~90 MB) |
| binder traffic in a full inflate+layout+draw | 8 `BINDER_WRITE_READ` attempts, 3 `ServiceManager` failures, **1 stub service call** (`IWindowManager.hasNavigationBar`) |
| noise | 1 398 × `System: No Compatibility callbacks set! Querying change …` from `AssetManager2` (harmless, one line to silence by giving `ApplicationInfo` a real `targetSdkVersion`) |

## 6. What M-B should take from this

1. **Ship the E2E seam (c) as designed.** Real `libhwui` + real Skia CPU + a `Bitmap` ACOS
   owns; the whole view pipeline above it costs 1.5 ms measure/layout and 0.45 ms draw.
2. **The binder peer M-B needs is small.** For a static first frame: one `IWindowManager`
   stub (1 method). Add `IDisplayManager` (display metrics/rotation), `IActivityManager`
   (start activity), `IPackageManager` (manifest/theme resolution, `ApplicationInfo`) as the
   app grows — each is the same shape as the proxy used here, so the M-B peer should be
   *real* service objects, not proxies (§3.2, 0.5 d each).
3. **Two things must move out of "startup" into "environment provisioning"**: the system
   font map (§3.3 — do it the zygote way, `SharedMemory` + `Typeface.setSystemFontMap`, so
   CJK/emoji work) and the font files themselves (`/system/fonts` is not in our sysroot).
4. **Theme resolution is a shared bug**, not a renderer bug: our renderer and ACOS-VM agree
   with each other to 2 px and disagree with the device by 38 px on one Button
   (`textAllCaps`). Fix it once, in the framework-side theme resolution; then both the ART
   seam and the ACOS-VM renderer move to ~0.997 SSIM on this app.
5. **`Drawable`s that assume hardware**: `RippleDrawable` is not alone (§3.4). The ACOS draw
   bridge needs a `isHardwareAccelerated()==false` policy and an audit; that is cheaper than
   making HWUI's GPU path work (E2E's (b), 8–15 d).

## 7. Not measured / open (with the cheapest next probe)

- **`textAllCaps` root cause** — I proved *which* attribute differs, not *why* the theme
  resolves it differently (device `Theme.DeviceDefault` + targetSdk 34 vs our explicit
  `Theme.DeviceDefault.Light`). Probe: dump `theme.obtainStyledAttributes(R.styleable.TextAppearanceButton)`
  + `context.getThemeResId()` on the device oracle vs here (0.5 d, one extra column in `compare.py`).
- **CJK/emoji text, font weights/styles** — our map has 8 named families all pointing at
  Roboto-Regular (§3.3b); unmeasured, expected wrong.
- **Non-trivial view trees** — only `counter` was attempted (the brief's target). The
  fixtures to try next, cheapest first: `hello-views` (LinearLayout+TextView, should pass
  unchanged), `form-validation` (EditText ⇒ `InputMethodManager`/`TextServicesManager`
  services ⇒ new stubs), `list-scroll` (`RecyclerView` needs AndroidX ⇔ `PathClassLoader`
  over the APK's `classes.dex` — the E1B loader, not the graphics seam), `dialog-menu`
  (`PopupWindow`/`WindowManager.addView` ⇒ real WM), `canvas-shapes` (custom `View.onDraw`,
  should pass unchanged).
- **Software `RippleDrawable`** — whether the `isHardwareAccelerated()` branch can be made
  to take over (0.5 d in libhwui) instead of our downgrade.
- **Damage/present** — nobody has pushed the `Bitmap` into `acos-wm`'s `FrameSink` yet
  (E2E's open item, 1–2 d).
- **Determinism under load / multiple frames** — only single-frame, 2×-warm was measured;
  `Choreographer` was never looped (we draw synchronously).
