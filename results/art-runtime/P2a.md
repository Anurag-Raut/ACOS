# P2a — `acos-art render-app`: an APK's first frame on real ART (productised E4H)

**Status: `counter.apk` meets every acceptance number; `hello-views.apk` does not
render — it stops on a real, newly-identified service gap (edit a text field ⇒
`InputMethodManager` ⇒ `Settings.Config` ⇒ `ContentProvider` ⇒
`IActivityManager`), reported as a structured `crash / INFLATE_FAILED`, not a
silent failure.** Owner: implementer (P2a), 2026-11-11. Basis: E4H
(`results/art-feasibility/E4H.md`), F (`results/art-runtime/F.md`),
`docs/ENGINEERING_POLICY.md`. Nothing committed; no ACOS crate outside
`crates/acos-art` touched; no sudo; every run under `tools/guarded.sh`.

```bash
# one APK -> frame.png + tree.json + structured result (inside the acos-art bwrap sandbox)
cargo run -q -p acos-art -- render-app --apk fixtures/build/apps/counter.apk --out DIR [--json]
cargo run -q -p acos-art -- build-app        # bake the guest dex + substrate shim only
```

## 0. Acceptance

| requirement | result |
|---|---|
| `render-app --apk fixtures/build/apps/counter.apk [--out DIR]` runs in the acos-art bwrap sandbox | **PASS** — one `dalvikvm64`, no zygote/system_server/servicemanager/`/dev/binder`, no GPU |
| produces `frame.png` + `tree.json` (class, id, text, bounds) | **PASS** — `results/art-runtime/p2a/counter/{frame.png,tree.json}` |
| structured `RunResult` with outcome `ok/crash/unsupported` + codes | **PASS** — `AppResult{outcome, code, frame_md5, nodes, draw_*, peak_rss_mb, logs, notes, argv}`; codes `NO_LAYOUT`, `INFLATE_FAILED`, `NO_FONTS`, `GUEST_ERROR`, plus the launcher's `ART_CRASH`/`ART_FATAL_*`/`TIMEOUT`/`UNSUPPORTED_SYSCALL` |
| works for `counter.apk` | **PASS** |
| works for `hello-views.apk` | **FAIL — blocked**, see §4 (`EditText` ⇒ IME/ContentProvider service peer). Reported as `crash / INFLATE_FAILED` with the full cause chain and where it stops |
| frame SSIM ≥ 0.99 vs the ART oracle (H's comparator) | **PASS** — **0.99685**, MAE 0.115/255, 98.50 % identical pixels |
| tree bounds match the oracle | **PASS for the app content tree: 4/4 exact** (root `LinearLayout`, `#count`, `#inc`, `#reset`); the 4 remaining rows are the *structural* nodes we deliberately do not build (`DecorView` root, activity `FrameLayout`, `ActionBar` title, `id/content`) and are reported as `missing_in_ours`, as in E4H |
| repeatable (same md5 twice) | **PASS** — `2fc7c91db19d870ff18d66acc8f7f497` twice in a row (and once more after a guest rebuild) |
| bake the guest dex/shims into a build subcommand | **PASS** — `acos-art build-app`; `render-app` builds them automatically, cached by source hash (`<data>/app/guest/classes.dex`, `<data>/app/libacosart.so`) |
| RAM < 90 %, `tools/guarded.sh`, no sudo | **PASS** — peak `VmHWM` **166 MB** guest-side (host: 60 GiB, 34 GiB in use); every run through `guarded.sh --mem 6G`; no privileged operation anywhere |

### The numbers, one place (`counter.apk`, 1080×2062 @ 420 dpi)

| measurement | value |
|---|---|
| SSIM vs ART oracle | **0.99685** (E4H's as-inflated variant: 0.99196) |
| MAE / identical pixels | 0.115 / 255, 98.50 % |
| tree, content nodes exact | **4/4** (E4H as-inflated: 3/4) |
| wall clock, whole command → PNG (incl. bwrap) | **121–146 ms** (guest 216 ms in E4H, i.e. no regression) |
| `draw_cold` / `draw_warm` | 366–457 µs / 19–25 µs |
| peak RSS (`VmHWM`) | **165–167 MB** (E4H: 241–247 MB; F's load-only baseline 163 MB) |
| PNG md5 | `2fc7c91db19d870ff18d66acc8f7f497` ×3 runs |
| logd entries (AOT-less JIT, imageless) | 506–514 |

## 1. What shipped

| path | what it is |
|---|---|
| `crates/acos-art/src/app.rs` | the product: `AppOpts`/`AppResult`/`render_app`, sandbox assembly, guest + shim build (hash-cached), APK metadata via `aapt2`, structured parsing of the guest verdict, font provisioning |
| `crates/acos-art/guest/java/dev/acos/art/AppMain.java` | the guest (E4H `Main` minus the probe rungs): graphics/runtime native registration → `systemMain` → `Resources`/`Theme` → `Typeface` seeding → stub services → inflate → measure/layout → software draw → PNG + tree JSON |
| `crates/acos-art/guest/java/dev/acos/art/RenderNat.java` | JNI bridge to the substrate shim (`register`, `sym`) |
| `crates/acos-art/shim/render_shim.c` | new: default-namespace `dlopen` of `libandroid_runtime.so`+`libhwui.so`, memfd `/dev/binder` stand-in, `register_*` bridge (E2D's shim, minus the property table — `libartshim.so` already serves `__system_property_*`) |
| `crates/acos-art/src/lib.rs` | +1 line: `pub mod app;` |
| `crates/acos-art/src/main.rs` | subcommands `render-app`, `build-app` + their options (`--apk`, `--package`, `--layout`, `--width`, `--content-height`, `--density`, `--text-allcaps`) |
| `results/art-runtime/p2a/counter/` | `frame.png`, `tree.json` (evidence) |

Build inputs are host SDK artefacts, resolved like the rest of the repo
(`ANDROID_SDK_ROOT`, else `~/Android/Sdk`): `platforms/android-34/android.jar`
(javac/d8), `build-tools/*/d8`, `build-tools/*/aapt2`,
`ndk/*/…/x86_64-linux-android34-clang`. Nothing is downloaded; `render-app`
fails with a `Tool` error naming the missing path if any of them is absent.

## 2. E4H findings applied (and why they are still needed)

1. **Graphics registration via libhwui's exported entry point.** One call to
   `android::register_android_graphics_classes(JNIEnv*)` (after the shim
   `dlopen`s `libandroid_runtime.so` + `libhwui.so` into the **default** namespace)
   plus 18 targeted `register_android_*`; the six Surface/SurfaceControl/
   GraphicBuffer/BLAST/SurfaceTexture entries stay **unregistered** — their
   register function runs the class initialiser whose `<clinit>` calls a native
   of the same class, giving a sticky `NoClassDefFoundError` (E4H §3.1).
2. **`ActivityThread.systemMain()` after `Looper.prepareMainLooper()`**, with the
   failing `getService("display"/"window")` calls logged and non-fatal.
3. **`Typeface` font map.** `sSystemFontMap` is a zygote artefact
   (`sSystemFontMapSharedMemory`), so a bare `dalvikvm64` has `sDefaultTypeface ==
   null` and `TextView`'s constructor NPEs in `Typeface.create(null,0)`. The guest
   rebuilds the default family (`Font$Builder` → `FontFamily$Builder` →
   `Typeface.createFromFamilies("sans-serif", …)`) and installs it with the @hide
   **`Typeface.setDefault(Typeface)`** — the only setter that updates libhwui's
   `gDefaultTypeface` as well; writing the Java field alone trips libhwui's
   `ASSERT src == nullptr && gDefaultTypeface == nullptr` on the first
   `measureText`. The font *files* must exist: `ensure_system_fonts()` hard-links
   the extracted boot-image font set into `<sysroot>/system/fonts`
   (`/system/etc/fonts.xml` is already there). They **cannot be bind-mounted** —
   bwrap cannot create a mount point inside the read-only `/system` bind
   (`bwrap: Can't create file /system/fonts: Read-only file system`) and
   `fonts.xml` uses absolute `/system/fonts/...` paths. Same mechanism as
   `research/art-feasibility/h-view/scripts/hroot.sh`; idempotent, 0 bytes copied
   (hard links), undo with `rm -rf <sysroot>/system/fonts`.
4. **Ripple on a software canvas.** `RippleDrawable.draw()` on a non-hardware
   `Canvas` kills the process with a **silent SIGSEGV** inside libhwui (no
   tombstone, no log line); it is the only drawable type in the fixture set that
   does. The guest replaces every `RippleDrawable` by its content layer
   (`getDrawable(0)`) before drawing — a ripple is a pressed-state animation and
   the static frame is its content layer (E4H §3.4). This is what makes the
   `Button`s render at all.
5. **No binder peer.** `View.<init>` → `ViewConfiguration` →
   `IWindowManager.hasNavigationBar()` is answered by a reflective `Proxy`
   installed as `WindowManagerGlobal.sWindowManagerService`.
6. **`textAllCaps`** (the one *deliberate* deviation from E4H's default): our
   theme resolution gives `Widget.Material.Button.textAllCaps=true` while the
   device oracle renders sentence case — a shared theme-resolution gap, not a
   renderer bug (E4H §4/§7). `render-app` therefore forces
   `textAllCaps=false` (`--text-allcaps device`, the default; `theme` restores
   the as-inflated behaviour). It is exactly what turns E4H's
   0.99196 / tree 3-of-4 into **0.99685 / tree 4-of-4**, i.e. into a match with
   the device. The proper fix is theme resolution from the APK manifest +
   targetSdk-dependent `values-v*` overrides (E4H §7, ~0.5 d).

## 3. How the seam is wired (differences from the spike)

- **One sandbox, two preloads.** `LD_PRELOAD=/data/shim/libartshim.so:/data/app/libacosart.so`:
  the existing freestanding art shim serves the property table and the logd sink,
  the new bionic shim serves the framework `dlopen` + native registration. The
  logd socket dir is bound over `/dev/socket` so `RunResult.logs` keeps working
  (506–514 structured entries per run); `Sandbox::with_shim(false)` is used only
  because `Sandbox` sets `LD_PRELOAD` itself to a single path — `app.rs` binds
  both libraries and the socket through the sandbox's public builder API, so
  `lib.rs` is unchanged apart from `mod app;`.
- **Panel geometry is an argument, not a constant** (`--width`,
  `--content-height`, `--density`), with the fixture default 1080×2062 @ 420
  (both `counter` and `hello-views` have `mAppBounds=Rect(0,128-1080,2337)` and an
  ActionBar at y=128..275, so the app content band is 275..2337 — 2062 rows).
- **Layout selection** is not hardcoded: `aapt2 dump resources` → `main`, then
  `activity_main`, then the first `layout/*`; `--layout` overrides. Package name
  comes from `aapt2 dump packagename` (override `--package`).
- **Artefacts are staged in the sandbox** (`/data/out/render-app`) and copied to
  `--out`, and `AppResult` carries `stdout`/`stderr` tails so a failure is
  diagnosable from the result alone (that is how §4 was found).

## 4. The blocker: `hello-views.apk` needs a *service peer*, not another proxy

`hello-views` inflates `LinearLayout + TextView + **EditText** + Button +
TextView`; everything but the `EditText` works. Cause chain, verbatim from
`AppResult.stdout` (`--json`):

```
CAUSE java.lang.IllegalStateException: IInputMethodManager is not available
    at android.view.inputmethod.InputMethodManager.createRealInstance(InputMethodManager.java:1396)
    at android.view.inputmethod.InputMethodManager.forContext(InputMethodManager.java:1474)
    at android.app.SystemServiceRegistry$30.getService(SystemServiceRegistry.java:538)
    at android.app.ContextImpl.getSystemService(ContextImpl.java:2212)
  ... from TextView.applySingleLine -> Editor.setTransformationMethod -> setText
```

Progress made against it in this task (both kept, both documented in the guest):

| step | symptom before | fix | outcome |
|---|---|---|---|
| 1 | `NullPointerException` at `Editor$ProcessTextIntentActionsHandler.<init>(Editor.java:7936)` — `Objects.requireNonNull(getPackageManager())` | instantiate `ApplicationPackageManager` over a Proxy'd `IPackageManager` (`PackageManager` is abstract, so a Proxy alone cannot stand in) | **PASS** — next blocker |
| 2 | `IllegalStateException: IInputMethodManager is not available` | put one object that is both `IBinder` and `com.android.internal.view.IInputMethodManager` into `ServiceManager.sCache["input_method"]` (`queryLocalInterface` returns itself, so `Stub.asInterface` accepts it) | **PASS** — next blocker |
| 3 | `NullPointerException: … IActivityManager.getContentProvider(…)` via `Settings$NameValueCache.getStringsForPrefix` → `Settings$Config.getStrings` (the IME reads `Settings.Config.IME_*`) | **none applied — this is where the 60-minute budget ends** | **BLOCKED** |

Why this is not a "one more proxy" problem: the IME path reads
`Settings.Config`, which goes through `ContentResolver` →
`IActivityManager.getContentProvider` + a real `ContentProvider` transaction.
Proxying `IActivityManager` to return a fake `ContentProviderHolder` would have
to answer `ContentProvider` calls with a real `Cursor` for the settings rows —
i.e. exactly the M-B peer that E4H §6.2 called out ("real service objects, not
proxies", ~0.5 d each) and that P2 (service host) is chartered to build. Faking
it now would be a *lower-fidelity* answer to a compatibility question, which
`docs/ENGINEERING_POLICY.md` forbids as a silent shortcut.

**Smallest next step (bounded, ~0.5–1 d, no architecture change):** a minimal
`Settings` provider answer in the peer: `IActivityManager.getContentProvider`
returning a local `ContentProvider` that serves `Settings.Config`'s
`getStringsForPrefix` for the `ime` namespace (a handful of key/value rows:
`enabled_input_methods`, `default_input_method`, `show_ime_with_hard_keyboard`).
The alternative already exists and is cheaper for *verification* purposes: the
IME is not needed for a static first frame, so the guest could catch the
`IllegalStateException` from `InputMethodManager` and fall back to a non-IME
`Editor` — but that changes framework behaviour, so it must be an explicit,
labelled mode (`--allow-no-ime`) and needs a decision, not a silent patch.

**Other gaps found, not yet blocking:** `hello-views`'s `EditText` also wants
`TextServicesManager` once the IME is up; `form-validation` / `text-input`
fixtures will hit the same wall. `list-scroll` needs the AndroidX class loader
(E1B) rather than a service.

## 5. Reproduction / evidence

```bash
# counter: full acceptance evidence (frame + tree in results/art-runtime/p2a/counter/)
cargo run -q -p acos-art -- render-app --apk fixtures/build/apps/counter.apk \
    --out results/art-runtime/p2a/counter
#   frame=…/frame.png tree=…/tree.json md5=2fc7c91db19d870ff18d66acc8f7f497 nodes=5 1080x2062@420
#   outcome=ok code=- wall=121ms draw_cold=373us draw_warm=19us peak_rss=166MB

# hello-views: structured failure (no frame/tree written, full cause chain in stdout)
cargo run -q -p acos-art -- render-app --apk fixtures/build/apps/hello-views.apk --json
#   outcome=crash code=INFLATE_FAILED … "Error inflating class android.widget.EditText"

# unit tests (17 pass)
cargo test -q -p acos-art
```

SSIM/MAE/pixel-exact and the node-by-node bounds diff come from **E4H's own
comparator**, imported unchanged (`research/art-feasibility/h-view/scripts/compare.py`:
`ssim`, `crop`, `oracle_nodes`, `diff_trees`) with the oracle paths pointed at
the app under test and `dy=275` (the content-band origin); `counter`:

```
{"app": "counter", "ours": [1080,2062], "ref": [1080,2062], "ssim": 0.99685,
 "mae": 0.115, "pixel_exact": 0.98497, "tree_exact": 4, "tree_total": 8}
```

Content-node bounds (ours = device − 275 on y, all identical to E4H's
`textAllCaps=false` variant and to the oracle after the shift):

| node | ours | device (oracle) |
|---|---|---|
| root `LinearLayout` | `[0,0][1080,2062]` | `[0,275][1080,2337]` ✅ |
| `#count` "0" | `[42,42][63,93]` | `[42,317][63,368]` ✅ |
| `#inc` "Increment" | `[42,93][273,219]` | `[42,368][273,494]` ✅ |
| `#reset` "Reset" | `[42,219][273,345]` | `[42,494][273,620]` ✅ |

## 6. Side effects, limits, and what is deliberately not done

- **Sysroot mutation (additive, idempotent, documented):**
  `<sysroot>/system/fonts/` now exists (hard links from
  `.cache/boot/android-34/fonts`). Without it *no* text renders — `TextView`'s
  constructor NPEs. A product build should ship the fonts in the export rather
  than create them on first use (`tools/art-sysroot.sh` is F2's file, so this task
  did not touch it). Undo: `rm -rf <sysroot>/system/fonts`.
- **No `RunOpts` reuse for the process:** `launcher::run()` hardcodes
  `LD_PRELOAD` to the single art shim, so `app.rs` spawns the sandbox itself
  (rlimits, `PDEATHSIG`, process-group kill, timeout, 5 ms poll) and reuses
  `launcher::classify()` for the outcome vocabulary — one classification
  implementation, not two.
- **`build-app` needs a host SDK** (javac/d8/aapt2 + NDK clang). The *runtime*
  needs only the sysroot; both builds are hash-cached, so re-runs are ~120 ms.
- **Not done (unchanged from E4H's open list):** theme resolution from the APK
  manifest (§2.6, the 0.997→1.0 gap); navigating/clicking (`frame N` after an
  input event); multiple frames from a `Choreographer` loop; damage/present into
  `acos-wm`'s `FrameSink`; CJK/emoji (the font map is default-family-only);
  `Drawable`s other than `RippleDrawable` that assume hardware.
- **Time:** stopped at the 60-minute checkpoint + one bounded (~35 min) repair
  pass that produced §4's two working service stubs and its exact blocking
  cause chain. No loop; `counter` is complete and repeatable.
