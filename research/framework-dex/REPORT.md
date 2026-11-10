# Research spike: running the real Android framework DEX on a Rust VM

- **Question owner:** lead hypothesis — do **not** rewrite the Android framework; load the
  real Android 14 / API 34 boot-classpath bytecode (`framework.jar`, `core-oj.jar`,
  `core-libart.jar`, okhttp, bouncycastle, …) and execute it on the Rust VM, supplying Rust
  implementations of the `native` methods and replacing system_server/Binder/SurfaceFlinger/HWUI
  with a thin Rust layer.
- **Time-box:** ~1 h. **Status:** complete for questions 1–5 below.
- **Cost:** zero downloads, zero sudo, zero mounts, zero emulator/container start. The
  host repo `/home/Qui/Work/android-container` was not written to.
- **Artefacts:** this directory (`research/framework-dex/`): `dexclosure.py` (the analysis
  script), `data/` (raw tool output), `REPORT.md`. Extracted binary artefacts were placed in
  `/tmp/acos-framework-dex/` (outside the repo) per instruction.

---

## (a) Verdict

**Feasible, and the real boot-classpath DEX is obtainable with no downloads** — the installed
`android-34/google_apis/x86_64` SDK system image contains a GPT disk whose `super` partition
holds the complete ext4 logical partitions, and every boot-classpath jar still contains real
`classes.dex` (all DEX v039; **no** cdex/compact-dex, **no** stripped `classes.dex`). Using only
already-installed tools (`dd`, `lpunpack`, `debugfs`, `unzip`, python3 stdlib) I extracted
`framework.jar` (5 dex, 40 MB), `services.jar` (system_server, 3 dex), and the ART/ICU/conscrypt
APEX payload jars (`core-oj`, `core-libart`, `core-icu4j`, `okhttp`, `bouncycastle`,
`apache-xml`, `conscrypt`). The total real boot classpath is **17 dex / 55.5 MiB, 41 829 classes,
328 352 defined methods, 5 607 `native` methods, 5 932 classes with `<clinit>`**. The static
Rust work-list (reachable `native` methods) is small enough to be a credible target: for
`System.out.println("x")` it is **152 native methods in 39 classes** (declared-only lower bound)
to **663 in 79 classes** (class-hierarchy over-approximation); for a minimal Activity
`<init>+attach+performCreate+setContentView(inflate LinearLayout/TextView/EditText/Button)+
measure/layout/draw` it is **612 native methods in 105 classes** (lower bound) to **2 270 in 300
classes** (upper bound). Of the 612 minimal-Activity natives, **175 are core-library, 419 are
`android.*`, 18 are other** — i.e. roughly two-thirds of the work is Android graphics/widget/
resource natives, which is exactly where a thin Rust layer (or layoutlib-style delegates) must
go. The single decisive open risk is **not** bytecode availability but dynamic `native`
coverage and the system-server coupling: a passive Activity launch touches at least
`IActivityManager, IPackageManager, IWindowSession, IWindowManager, IDisplayManager,
IAccessibilityManager, IInputMethodManager, IGraphicsStats, IServiceManager,
IActivityClientController, IContentProvider` (193 interface types are reachable from
`ActivityThread.main`+`ViewRootImpl.setView` alone), each of which currently assumes a live
Binder peer. layoutlib proves the "real framework, no system_server, delegate the natives"
shape works in the JVM world (real `Activity.class`/`View.class` bytecode + 66 `*_Delegate`
classes), so the hypothesis is credible, but ACOS would be doing it against real **DEX** with a
Rust VM instead of JVM `.class`.

---

## (b) Answers with evidence

### Q1 — Sourcing without downloads

**Tools already installed** (`command -v`): `simg2img`, `lpunpack`, `lpdump`, `debugfs`,
`unzip`, `bsdtar`, `file`, `python3` (3.14), `javap`, and SDK `dexdump`, `aapt2`, `d8`,
`apksigner`, `zipalign` (not on `PATH`, under `~/Android/Sdk/build-tools/35.0.0/`).
**Missing:** `7z`, `fsck.erofs`/`dump.erofs`/`erofsfuse` (not needed — the images are ext4).

**Image structure.** `system.img` is a whole-disk GPT image, not an ext4/erofs image. `file`
reports a protective MBR + GPT; parsing the GPT gives:

| partition | start LBA | sectors | size | contents |
|---|---|---|---|---|
| `vbmeta` | 2048 | 2048 | 1 MiB | AVB vbmeta |
| `super` | 4096 | 8 404 992 | 4104 MiB | Android dynamic-partition metadata + logical partitions |

`super` holds logical partitions `system` (1 737 848 sectors ≈ 848 MiB), `system_dlkm`,
`system_ext`, `product`, `vendor` (from `lpdump`). The `system` logical partition is **ext4**
(superblock magic `0xEF53` at offset 0x438). `vendor.img` is a separate GPT image with a
`vendor` ext4 partition. No erofs appeared anywhere relevant.

**Extraction route that worked (unprivileged, no mount):**

1. `dd` the `super` extent out of the disk image (sparse) → `/tmp/acos-framework-dex/super.img`.
2. `lpunpack -p system -p system_ext -p product -p system_dlkm` → ext4 partition images.
3. `debugfs -R 'dump …'` to read files out of the ext4 `system.img` (read-only, no mount).
4. APEX: `.apex` is a ZIP. `com.google.android.art.apex` and `…conscrypt.apex` are
   *compressed* APEX (contain `original_apex`, itself a ZIP); `com.android.i18n.apex` and
   `com.android.runtime.apex` contain `apex_payload.img` directly. Each payload is ext4 and
   was read with `debugfs` (`javalib/*.jar`).

**What the route yielded** (`/system/framework` and APEX `javalib`):

| source | file | real `classes.dex`? |
|---|---|---|
| `/system/framework` | `framework.jar` (40 011 934 B, **5 dex**) | yes |
| `/system/framework` | `framework-graphics.jar`, `ext.jar`, `telephony-common.jar`, `voip-common.jar`, `ims-common.jar` | yes |
| `/system/framework` | `services.jar` (20 621 436 B, **3 dex**, system_server) | yes |
| `apex/com.google.android.art` | `core-oj.jar`, `core-libart.jar`, `okhttp.jar`, `bouncycastle.jar`, `apache-xml.jar`, `service-art.jar` | yes |
| `apex/com.android.i18n` | `core-icu4j.jar` | yes |
| `apex/com.google.android.conscrypt` | `conscrypt.jar` | yes |

All 24 extracted dex files are `dex\n039` except the fixture app (`038`); no `cdex`/compact dex
was found (`grep -rl cdex` → none). The image *also* ships `boot-*.vdex`/`*.odex` (13 vdex in
`/system/framework`), but they are not needed because the jars are **not** dex-stripped. This is
a `userdebug` Google-APIs image; see "Could not determine" for the production-build caveat.

**Alternative routes checked (all rejected as sources of runnable DEX):**

| route | finding |
|---|---|
| `~/Android/Sdk/sources/` | **`android-34` is absent**; only `android-36` sources are installed. |
| `~/Android/Sdk/platforms/android-34/android.jar` | **Stubs only** — `strings` on `android/app/Activity.class` contains `Stub!`; 26 MB. Not executable. |
| layoutlib (`/opt/android-studio/plugins/design-tools/lib/layoutlib.jar`) | 121 MB, 30 103 entries, **25 223 JVM `.class`** including *real* framework classes (`android/app/Activity.class` 137 KB with a real body: `aload_0; invokevirtual getWindow; invokevirtual Window.setContentView; invokevirtual initWindowDecorActionBar; return`), plus **66 `*_Delegate` classes** and 93 `com/android/layoutlib/bridge/` classes. This is **JVM bytecode, not DEX**; valuable as prior art for "real framework without system_server", not as a DEX source. `~/Android/Sdk/platforms/android-28/data/layoutlib.jar` is only 5 entries (a stub). |
| APEX `original_apex` / `apex_payload.img` | done, see above |

**Authorization needed:** none for what was done. Installing `7z`/erofs tools, or downloading
AOSP sources, is unnecessary. No user authorization is required to complete a DEX run, only the
usual decision about *distributing* core-library jars (Q5).

### Q2 — Inventory (boot classpath and per jar)

**Full boot classpath inventory** (17 dex, excluding `services.jar`):
`total 58 217 452 B (55.5 MiB); DEX v039; 41 829 classes; 328 352 defined methods;
5 607 native methods; 5 932 `<clinit>` methods / 5 932 classes with `<clinit>`.**

Per-jar (`dexclosure.py jarinv …`, full output in `data/jarinv.txt`):

| jar | dex | dex bytes | classes | methods | native | clinit classes |
|---|---:|---:|---:|---:|---:|---:|
| core-oj | 1 | 5 579 712 | 4 053 | 36 686 | **601** | 810 |
| core-libart | 1 | 572 248 | 468 | 4 114 | **329** | 74 |
| core-icu4j | 1 | 2 943 700 | 1 871 | 14 076 | 66 | 635 |
| okhttp | 1 | 405 312 | 241 | 2 429 | 0 | 53 |
| bouncycastle | 1 | 1 479 136 | 1 189 | 7 836 | 0 | 186 |
| apache-xml | 1 | 1 192 504 | 679 | 6 427 | 0 | 63 |
| conscrypt | 1 | 466 672 | 347 | 2 863 | 294 | 68 |
| **framework** | **5** | 39 961 748 | 29 344 | **224 883** | **4 311** | 3 676 |
| framework-graphics | 1 | 3 528 | 1 | 16 | 0 | 0 |
| ext | 1 | 293 836 | 108 | 1 014 | 0 | 30 |
| telephony-common | 1 | 3 898 980 | 2 546 | 20 132 | 0 | 245 |
| voip-common | 1 | 821 916 | 600 | 4 928 | 6 | 38 |
| ims-common | 1 | 598 160 | 382 | 2 948 | 0 | 54 |
| `services.jar` (system_server, not boot) | 3 | 20 586 180 | 13 543 | 105 295 | 345 | 1 029 |

Top classes by native-method count (see `data/jarinv.txt` for the full top-40 per jar):

- **framework.jar:** `GLImpl` 250, `GLES20` 194, `GLES30` 164, `RenderScript` 128, `GLES10` 124,
  `GLES11Ext` 121, `SurfaceControl` 109, `GLES31` 100, `AudioSystem` 99, `RenderNode` 89,
  `Paint` 85, `HardwareRenderer` 72, `AssetManager` 55, `Parcel` 51, `MotionEvent` 51, …
- **core-oj:** `jdk/internal/misc/Unsafe` 61, `sun/misc/Unsafe` 60,
  `sun/nio/fs/UnixNativeDispatcher` 47, `java/lang/Class` 32, `VarHandle` 31, `sun/nio/ch/Net` 30,
  `reflect/Field` 24, `Character` 23, `Math` 23, `StrictMath` 20, `System` 17, `String` 11…
- **core-libart:** `libcore/io/Linux` 134, `VMRuntime` 48, `VMDebug` 27, `libcore/io/Memory` 25,
  `DexFile` 21, `ExpatParser` 11, `libcore/icu/ICU` 10, `ZygoteHooks` 7, `NativeBN` 7, …
- **services.jar:** `NativeInputManagerService$NativeImpl` 78, `GnssNative` 49, …
- **conscrypt:** `NativeCrypto` 294 (all).

**DEX formats:** v039 everywhere in the boot classpath. `dexdump` reports `hiddenapi` flags
(e.g. `MAX-TARGET-O`, `SDK`, `BLOCKED`) embedded in the dex; these are link-time policy metadata,
not a format blocker — a Rust VM may implement or ignore them. No compact-dex, no cdex, no
pre-optimised/stripped dex encountered.

**BOOTCLASSPATH order** (authoritative, from `derive_classpath` inputs in the image):

From `/apex/com.android.art/etc/classpaths/bootclasspath.pb` (ART APEX):
```
1  /apex/com.android.art/javalib/core-oj.jar
2  /apex/com.android.art/javalib/core-libart.jar
3  /apex/com.android.art/javalib/okhttp.jar
4  /apex/com.android.art/javalib/bouncycastle.jar
5  /apex/com.android.art/javalib/apache-xml.jar
```
From `/system/etc/classpaths/bootclasspath.pb`:
```
6  /system/framework/framework.jar
7  /system/framework/framework-graphics.jar
8  /system/framework/ext.jar
9  /system/framework/telephony-common.jar
10 /system/framework/voip-common.jar
11 /system/framework/ims-common.jar
12 /apex/com.android.i18n/javalib/core-icu4j.jar
```
`systemserverclasspath.pb` adds `/apex/com.android.art/javalib/service-art.jar`,
`/system/framework/com.android.location.provider.jar`, `/system/framework/services.jar`.
`init.environ.rc` no longer sets `BOOTCLASSPATH` (it sets `ANDROID_ART_ROOT=/apex/com.android.art`,
`ANDROID_I18N_ROOT=/apex/com.android.i18n`, `ANDROID_TZDATA_ROOT=/apex/com.android.tzdata`).
`conscrypt.jar` is **not** on the boot classpath; it is an APEX-provided jar loaded by the
framework.

### Q3 — Minimal-Activity dependency estimate

**Method / approximation.** `research/framework-dex/dexclosure.py` is a stdlib-only DEX parser +
conservative call-graph closure. It parses the real DEX (headers, class_data, code items),
decodes instruction sizes from the Dalvik opcode table mirrored in
`crates/acos-dex/src/opcode.rs`, and follows `invoke-*`, `new-instance`, and static field access.
Approximation:
- **`VIRT=0` ("lower bound", declared-only):** a virtual/interface `invoke` adds only the
  declared method (plus its class initialiser). This is a strict **under**-approximation of
  dynamic dispatch.
- **`VIRT=1` ("upper bound", CHA):** adds every same-name+proto method declared by a transitive
  subclass / implementor of the declared receiver type. This over-approximates.
- Class initialisers are added for `new-instance`, `sget*`/`sput*`, and every invoked method's
  declaring class. `const-class` is treated as load-only (does not trigger `<clinit>`).
- `invoke-super` follows the superclass chain; `invoke-direct/static` is exact.
- Payloads (`fill-array-data`, `packed-switch`, `sparse-switch`) are detected and skipped so a
  linear decode does not mis-parse them.

**Roots used.** println: `Ljava/io/PrintStream;->println(Ljava/lang/String;)V` +
`Ljava/lang/System;-><clinit>()V` over the 7 core jars. Activity: all 26 fixture roots
(the 9 `MainActivity`/`MainActivity$1` lifecycle methods, `Activity.<init>`,
`Activity.attach(…)`, `performCreate`, `setContentView(I)/(View)`, `LayoutInflater.inflate`,
`LinearLayout/TextView/EditText/Button.<init>(Context)`, `View.measure/layout/draw`,
`ViewRootImpl.setView/performTraversals`, `Log.i`) over the full boot classpath + the fixture dex
(`fixtures/build/apps/hello-views.apk`, 6 classes / 18 methods with code).

**Results** (raw `DUMP_NATIVES` listings in `data/`):

| closure | methods | classes | bytecode bytes | natives | core | android | other |
|---|---:|---:|---:|---:|---:|---:|---:|
| println (lower, VIRT=0) | 4 947 | 1 004 | 387 398 | **152** | 139 | 1 | 12 |
| println (upper, CHA) | 39 596 | 6 520 | 2 504 470 | **663** | 639 | 1 | 23 |
| **minimal Activity (lower, VIRT=0)** | **15 740** | **3 067** | **1 181 908** | **612** | **175** | **419** | **18** |
| minimal Activity (upper, CHA) | 174 733 | 30 527 | 10 595 152 | **2 270** | 756 | 1 447 | 67 |

**Rust work-list, minimal Activity (lower bound, 612 natives):** core-library dominates the
*count* of separate classes but `android.*` dominates the *work*:

- core (175 native methods): `java/lang/Class` 21, `Character` 13, `VMRuntime` 12,
  `util/zip/ZipFile` 12, `java/lang/Math` 11, `java/lang/String` 10, `MatcherNative` 9,
  `sun/misc/Unsafe` 9, `NativeBN` 6, `java/lang/System` 5, `Thread` 5, `PatternNative` 4,
  `DexFile` 4, `Object` 4, `reflect/Field` 4, … (43 core classes total).
- `android.*` (419 native methods): `graphics/RenderNode` 54, `view/MotionEvent` 37,
  `content/res/AssetManager` 27, `graphics/Paint` 23, `graphics/Canvas` 17,
  `view/SurfaceControl` 17, `graphics/HardwareRenderer` 16, `graphics/Matrix` 15,
  `os/Parcel` 15, `graphics/Bitmap` 11, `graphics/text/LineBreaker` 10, `graphics/Region` 10, …
  (58 `android.*` classes total).
- other (18): `jdk/internal/misc/Unsafe` 12, plus a handful.

The most important natives on the *draw* path are exactly `RenderNode` (the HWUI display list),
`HardwareRenderer` (`nCreateProxy`, `nSyncAndDrawFrame`, `nSetSurfaceControl`), `Canvas`,
`Paint` (text measurement via Minikin/HarfBuzz), `Matrix`, `Bitmap`, `SurfaceControl`,
`AssetManager`/`ApkAssets` (resource resolution), `Parcel` (Binder), `MotionEvent` (input).
A minimal renderer that produces a correct *semantic* tree need not implement all of them, but a
*pixel/draw* path would.

The **full class-by-class native list** for `println_lower` (39 classes) and `activity_lower`
(105 classes) is appended in the Appendix and written verbatim by the script to
`data/native_class_summary.txt`; the per-method signatures are in `data/println_lower.txt`,
`data/activity_lower.txt`, `data/activity_upper.txt` (`NATIVE\t<class>\t<name>\t<proto>`).

**Context:** the minimal-Activity lower bound (612) is 10.9 % of all 5 607 boot-classpath
natives; the println lower bound (152) is 2.7 %.

### Q4 — system_server / Binder coupling on the minimal path

Derived from the actual **API-34 dex** by scanning the closure of `ActivityThread.main`,
`ActivityThread.handleBindApplication`, `ActivityThread.performLaunchActivity`,
`ViewRootImpl.setView` and walking only invokes whose declaring class is an interface
(`dexclosure.py aidl`; raw output `data/aidl.txt`). **193 interface types** are reachable;
the service-relevant ones and the calls made on this path are:

| AIDL interface | calls made on the minimal path | canned in-process Rust stub sufficient? |
|---|---|---|
| `android.app.IActivityManager` | `attachApplication(IApplicationThread,J)`, `finishAttachApplication(J)`, `registerStrictModeCallback`, `handleApplicationStrictModeViolation`, `publishContentProviders`, `refContentProvider`, `removeContentProvider`, `setRenderThread`, `showWaitingForDebugger`, `waitForNetworkStateUpdate`, `getMimeTypeFilterAsync`, `addPackageDependency` | **Yes for a canned launch** — the app only needs `attachApplication` to return a bind-data parcel; no real scheduling is needed for a single deterministic task. |
| `android.app.IActivityClientController` | `getDisplayId(IBinder)I` | **Yes** — return a fixed display id. |
| `android.content.pm.IPackageManager` | `getApplicationInfo`, `getInstrumentationInfoAsUser`, `getPackagesForUid`, `notifyPackageUse` | **Yes** if the Rust layer parses the APK/manifest itself and returns canned `ApplicationInfo`/`ActivityInfo`; no real package DB. |
| `android.view.IWindowSession` | `addToDisplayAsUser(...)`, `getWindowId`, `outOfMemory`, `pokeDrawLock`, `setOnBackInvokedCallbackInfo`, `updatePointerIcon` | **Mostly** — `addToDisplayAsUser` is the real "create a surface" call; an offscreen Rust compositor can return a synthetic `IWindow`/`InputChannel`/`InsetsState`. |
| `android.view.IWindowManager` | `getCurrentAnimatorScale`, `hasNavigationBar`, `showStrictModeViolation`, `useBLAST` | **Yes** — constant returns. |
| `android.hardware.display.IDisplayManager` | `getDisplayIds`, `getDisplayInfo`, `getOverlaySupport`, `getPreferredWideGamutColorSpaceId`, `registerCallbackWithEventMask` | **Yes** — one canned virtual display. |
| `android.view.accessibility.IAccessibilityManager` | `addClient`, `addAccessibilityInteractionConnection`, `getFocusColor`, `getFocusStrokeWidth`, `getRecommendedTimeoutMillis`, `setAccessibilityWindowAttributes` | **Yes** — default disabled + constants; enables the semantics/tree path. |
| `com.android.internal.view.IInputMethodManager` | `startInputOrWindowGainedFocus`, `hideSoftInput`, `isImeTraceEnabled`, `acceptStylusHandwritingDelegation`, `startStylusHandwriting` | **Yes** — a no-op IME stub (input is injected by the Rust layer, not the IME service). |
| `android.view.IGraphicsStats` | `requestBufferForProcess(String, IGraphicsStatsCallback)` | **Yes** — return null/closed fd; only used for frame stats. |
| `android.os.IServiceManager` | `getService(String)` | **Yes** — this is the single chokepoint; a Rust in-process `ServiceManager` map can serve all of the above. |
| `android.content.IContentProvider` | `asBinder`, `call`, `getTypeAsync`, `query` | **Only if the app touches a ContentProvider/ContentResolver**; not on the pure-View path. |
| `android.view.IWindowId`, `com.android.internal.inputmethod.IInputMethodSession` | `asBinder`, `displayCompletions` | **Yes** — trivial stubs. |

**Source references** (note: **android-34 sources are not installed**; the only installed tree is
`~/Android/Sdk/sources/android-36`, so these line numbers are android-36 and will differ slightly
on 34 — they are indicative of the call sites, not byte-exact for API 34):

- `android/app/ActivityThread.java:8741-8743` — `ActivityManager.getService().attachApplication(mAppThread, startSeq)`
- `android/app/ActivityThread.java:2953-2958` — `getPackageManager()` → `IPackageManager.Stub.asInterface`
- `android/app/ActivityManager.java:5779` — `public static IActivityManager getService()`
- `android/app/ActivityTaskManager.java:149` — `public static IActivityTaskManager getService()` (used when the app *starts* an activity, not on the passive-launch path)
- `android/view/WindowManagerGlobal.java:239` — `getWindowSession()`; `:215` — `getWindowManagerService()`
- `android/view/ViewRootImpl.java:1633` — `mWindowSession.addToDisplayAsUser(...)`; `:3842` — `relayoutWindow(...)`
- `android/hardware/display/DisplayManagerGlobal.java:271` — `mDm.getDisplayInfo(displayId)`
- `android/view/accessibility/AccessibilityManager.java:253` — `private IAccessibilityManager mService`
- `android/view/inputmethod/InputMethodManager.java:522` — `final IInputMethodManager mService`; `:970` — `startInputOrWindowGainedFocus`
- `android/graphics/HardwareRenderer.java:1355` — `IGraphicsStats.Stub.asInterface(binder)`; `:1458` — `requestBufferForProcess(...)`

**How layoutlib solves the same problem (prior art).** layoutlib runs the **real framework
`View`/`Activity`/resource code compiled to JVM `.class`** directly on a JVM, with **no
system_server, no Binder, and no `app_process`**. It supplies two things: (1) a `com.android.layoutlib.bridge.*`
"bridge" that implements the platform services the View system calls (window/surface, config,
resources, `Layout`), and (2) per-class `*_Delegate` classes (66 in the bundled jar, e.g.
`ActivityThread_Delegate`, `IUiModeManager_Stub_Delegate`, `ApplicationPackageManager$…Delegate`)
that replace `native`/service-dependent behaviour. In other words layoutlib validates exactly the
ACOS shape — *execute the real framework implementation, replace the native/system-server edge,
not the framework* — but it does so above a JVM, not above a DEX interpreter, and it is a design/
preview renderer, not an app runtime. The number to note: layoutlib needed ~66 delegate classes
for its scope, versus 612 (lower) / 2 270 (upper) reachable native methods here — a different
unit, but the same order-of-magnitude message.

### Q5 — Licences (input for a human decision, not legal advice)

| component | provenance in this image | licence (as shipped / upstream) |
|---|---|---|
| `framework.jar`, `framework-graphics.jar`, `ext.jar`, `telephony-common.jar`, `voip-common.jar`, `ims-common.jar` | `/system/framework` | Apache-2.0 (AOSP); `ext.jar` aggregates third-party modules with their own licences |
| `core-oj.jar` | ART APEX | AOSP libcore mix: OpenJDK-derived parts are GPLv2 **with the Classpath exception**; AOSP/Harmony parts are Apache-2.0. This is the licence-sensitive component. |
| `core-libart.jar` | ART APEX | predominantly Apache-2.0 (Apache Harmony-derived) plus OpenJDK-derived parts |
| `core-icu4j.jar` | i18n APEX | Unicode/ICU licence (permissive) |
| `okhttp.jar` | ART APEX | Apache-2.0 |
| `bouncycastle.jar` | ART APEX | MIT (Bouncy Castle) |
| `apache-xml.jar` | ART APEX | Apache-2.0 |
| `conscrypt.jar` | conscrypt APEX | Apache-2.0 (bundles OpenSSL; OpenSSL licence) |
| `services.jar` | `/system/framework` | Apache-2.0 |

Evidence: the system image ships a 19.6 MB aggregate `NOTICE.txt`; keyword counts are
Apache-2.0 ≈ 2 237, GNU GPL ≈ 2 583, “Classpath” ≈ 650, BSD ≈ 1 469, ICU/Unicode ≈ 8 000+,
MPL ≈ 12 601. The 650 Classpath mentions confirm the GPLv2+Classpath core-library material is
present.

**Does merely loading the unmodified, user-installed jars at runtime (not redistributing) change
the analysis?** The honest answer is *partly, and it needs legal review*:

- The **Classpath exception** exists precisely to allow a program to link/run GPL-licensed
  JVM library code without the linking program falling under GPLv2. Running unmodified core
  library bytecode on a different VM is the normal intended use and should not force ACOS itself
  to be GPL. **But ACOS replaces `native` methods that are part of those very classes, which is
  closer to "modifying/linking" than to a pristine JVM.** Reasonable readings differ.
- If ACOS only **reads** jars from a system image the user already installed, there is **no
  redistribution** by ACOS, so notice/source-offer obligations are not triggered by ACOS. If ACOS
  ever **ships** an image/jar bundle, redistribution obligations (source offer, notices) do apply.
- The aggregate NOTICE and the ART/i18n/conscrypt APEX NOTICE files exist and should be carried
  through *if* redistribution ever happens.

**Flagged uncertainty:** this spike did not read the per-file licence headers inside the jars and
is not a legal opinion. A human/licensing review is required before any distribution; a
“runtime-only, user-provided image” posture is materially safer than bundling.

---

## (c) Commands used (reproduction)

Environment / survey:
```bash
free -g; nproc; df -h /tmp
for t in simg2img lpunpack 7z debugfs fsck.erofs dump.erofs dexdump unzip bsdtar python3; do command -v $t; done
ls -la /home/Qui/Android/Sdk/system-images/android-34/google_apis/x86_64/
ls /home/Qui/Android/Sdk/platforms/ /home/Qui/Android/Sdk/sources/
file system.img vendor.img ramdisk.img userdata.img
```

GPT parse + super extraction + logical partitions:
```bash
cd /home/Qui/Android/Sdk/system-images/android-34/google_apis/x86_64/
python3 - <<'EOF'   # parse GPT, print partition name/type/start/size
# (see REPORT; reads GPT header at 512, entries at entries_lba)
EOF
dd if=system.img of=/tmp/acos-framework-dex/super.img bs=1M skip=2 count=4104 conv=sparse
lpdump /tmp/acos-framework-dex/super.img
lpunpack -p system -p system_ext -p product -p system_dlkm super.img lp
```

ext4 file extraction:
```bash
debugfs -R "ls -l /system/framework" lp/system.img
debugfs -R "dump /system/framework/framework.jar sys/framework/framework.jar" lp/system.img
# (same for ext.jar, framework-graphics.jar, services.jar, telephony-common.jar, …)
debugfs -R "dump /system/etc/classpaths/bootclasspath.pb sys/classpaths/bootclasspath.pb" lp/system.img
```

APEX extraction:
```bash
unzip -o sys/apex/com.android.i18n.apex apex_payload.img -d i18n
unzip -o sys/apex/com.google.android.art.apex original_apex -d art
unzip -o art/original_apex apex_payload.img -d art2
debugfs -R "dump /javalib/core-oj.jar sys/framework/core-oj.jar" ap/art_payload.img
# (core-libart, okhttp, bouncycastle, apache-xml, service-art, core-icu4j, conscrypt)
```

Inventory / closure:
```bash
DD=/home/Qui/Android/Sdk/build-tools/35.0.0/dexdump
$DD -f dex/framework-graphics/classes.dex
for j in sys/framework/*.jar; do unzip -o -q "$j" 'classes*.dex' -d "dex/$(basename $j .jar)"; done
python3 research/framework-dex/dexclosure.py inventory <dex...>
python3 research/framework-dex/dexclosure.py jarinv <dex...>              # per-jar table
VIRT=0 python3 research/framework-dex/dexclosure.py closure --roots '<…||…>' <dex...>   # lower bound
DUMP_NATIVES=1 python3 research/framework-dex/dexclosure.py closure --roots '<…||…>' <dex...>  # + native list
VIRT=0 python3 research/framework-dex/dexclosure.py aidl --roots '<ActivityThread.main||ViewRootImpl.setView||…>' <core+framework+services dex>
# in-repo example (works):
cargo run -q -p acos-dex --example dexdump -- /tmp/acos-framework-dex/fixture/classes.dex
```

layoutlib / stub checks:
```bash
unzip -l /opt/android-studio/plugins/design-tools/lib/layoutlib.jar | head
javap -classpath /opt/android-studio/plugins/design-tools/lib/layoutlib.jar -c android.app.Activity
unzip -p ~/Android/Sdk/platforms/android-34/android.jar android/app/Activity.class | strings | grep -m3 'Stub!'
```

Source-call-site greps (android-36, flagged):
```bash
S=~/Android/Sdk/sources/android-36
grep -nE 'IActivityManager|attachApplication\(' $S/android/app/ActivityThread.java
grep -nE 'IPackageManager|getPackageManager\(\)' $S/android/app/ActivityThread.java
grep -n 'static IActivityManager getService()' $S/android/app/ActivityManager.java
grep -nE 'IWindowSession|getWindowSession' $S/android/view/WindowManagerGlobal.java
grep -nE 'addToDisplayAsUser|relayoutWindow' $S/android/view/ViewRootImpl.java
grep -nE 'IDisplayManager' $S/android/hardware/display/DisplayManagerGlobal.java
grep -nE 'IAccessibilityManager' $S/android/view/accessibility/AccessibilityManager.java
grep -nE 'IInputMethodManager' $S/android/view/inputmethod/InputMethodManager.java
grep -nE 'IGraphicsStats|requestBufferForProcess' $S/android/graphics/HardwareRenderer.java
```

Final verification:
```bash
git -C /home/Qui/Work/android-container status --short   # → empty (exit 0)
du -sh /tmp/acos-framework-dex
```

---

## (d) What could NOT be determined, and why

1. **android-34 framework source line numbers.** `~/Android/Sdk/sources/` contains only
   `android-36`; the requested `android-34` tree is absent and no download was permitted. The
   `file:line` references are therefore android-36 and are indicative only. The *interface and
   method* facts (`data/aidl.txt`) come from the actual API-34 dex and are exact.
2. **Exact dynamic-dispatch reachability.** No profiling was run (no emulator/docker). The
   reported range `[declared-only, CHA]` brackets the truth; the real value depends on runtime
   receiver types. For println, only two roots means the declared-only bound is close to reality;
   for Activity the spread is 15 740 → 174 733 methods.
3. **Runtime `native` call counts.** All native lists are *static reachability*, not executed
   call counts. Many reachable natives (e.g. `conscrypt/NativeCrypto` on the CHA bound) will not
   actually run in a headless semantic task.
4. **Production-build dex stripping.** This `userdebug` Google-APIs image ships full
   `classes.dex` in the boot jars. On a production `user` build, system/framework jars may be
   dex-stripped with dex only in `vdex`/`odex`; that route was not verified (no production image
   on disk) and would be a real blocker if true. The emulator image here is a valid oracle.
5. **Hidden-API enforcement semantics.** The dex carries `hiddenapi` flags; whether ACOS should
   honour them is a policy decision, not measured here.
6. **Any actual Binder IPC / system_server behaviour.** No process was launched; the coupling
   table is a static call inventory, not an observed trace.
7. **Exact licence of each embedded sub-module.** Not reviewed file-by-file; the aggregate NOTICE
   only proves the categories are present.
8. **layoutlib API level.** `javap -constants android.os.Build$VERSION` did not expose a usable
   `SDK_INT` in the bundled layoutlib jar; its exact platform level was not pinned.

---

## (e) Recommended next experiment

**Implement the `System.out.println` native work-list in Rust and execute the unmodified
`core-oj` DEX for that path.** Concretely:

1. Build a DEX interpreter slice that can execute the **4 947-method declared-only println
   closure** (or at least the `PrintStream.println → print → write → newLine` chain and
   `System.<clinit>`).
2. Implement the **152 reachable natives** (39 classes) as Rust natives, with a structured
   `unsupported` result for anything not yet done — never a fake pass.
3. Differential-test stdout/bytes against the `jvm-reference` tier (OpenJDK 17 running the same
   `System.out.println`), then, if the boot jars load cleanly, use the extracted
   `framework.jar` dex as the next milestone.
4. Only then attempt the **minimal Activity closure** and grow the native work-list in priority
   order: (a) `java/lang/Class`, `String`, `Character`, `Math`, `System`, `Thread`,
   `jdk/internal/misc/Unsafe`, `sun/misc/Unsafe`; (b) `AssetManager`/`ApkAssets` + resources;
   (c) `RenderNode`/`HardwareRenderer`/`Canvas`/`Paint`/`Matrix`/`Bitmap`; (d) `Parcel`/Binder
   behind an in-process Rust `ServiceManager`.
5. Measure and report the point of first `unsupported` native per fixture — that is the honest
   progress metric, not a raw class count.

A useful cheap follow-up is to re-run `dexclosure.py aidl` with `VIRT=1` to bound how many
*additional* service interfaces become reachable under dynamic dispatch, and to add a
`registerNatives`/JNI-table inventory per native class to size the Rust JNI surface.

---

## Appendix — native classes grouped by class (generated)

Full per-method signatures are in `data/println_lower.txt`, `data/println_upper.txt`,
`data/activity_lower.txt`, `data/activity_upper.txt` (`NATIVE\t<class>\t<name>\t<proto>`),
regenerated by:
```bash
DUMP_NATIVES=1 python3 research/framework-dex/dexclosure.py closure --roots '<…>' <dex…>
```

```
### println_lower: 39 native classes, 152 native methods
  CORE       20  Ljava/lang/Class;
  CORE       12  Ljava/util/zip/ZipFile;
  CORE       10  Ljava/lang/String;
  CORE        9  Ljava/lang/Character;
  CORE        9  Lsun/misc/Unsafe;
  CORE        8  Ldalvik/system/VMRuntime;
  CORE        7  Lcom/android/icu/util/regex/MatcherNative;
  CORE        6  Llibcore/math/NativeBN;
  CORE        5  Ljava/lang/System;
  CORE        4  Lcom/android/icu/util/regex/PatternNative;
  CORE        4  Ldalvik/system/DexFile;
  CORE        4  Ljava/lang/Object;
  CORE        3  Ljava/lang/Thread;
  CORE        3  Llibcore/icu/ICU;
  CORE        2  Lcom/android/icu/charset/NativeConverter;
  CORE        2  Lcom/android/icu/util/CaseMapperNative;
  CORE        2  Ldalvik/system/VMStack;
  CORE        2  Ljava/io/ObjectInputStream;
  CORE        2  Ljava/lang/Double;
  CORE        2  Ljava/lang/Float;
  CORE        2  Ljava/lang/Math;
  CORE        2  Ljava/lang/StringFactory;
  CORE        2  Ljava/lang/VMClassLoader;
  CORE        2  Ljava/lang/reflect/Array;
  CORE        2  Ljava/lang/reflect/Constructor;
  CORE        2  Ljava/lang/reflect/Field;
  CORE        1  Ldalvik/system/BaseDexClassLoader;
  CORE        1  Ljava/io/UnixFileSystem;
  CORE        1  Ljava/lang/Throwable;
  CORE        1  Ljava/lang/ref/Reference;
  CORE        1  Ljava/lang/reflect/Executable;
  CORE        1  Ljava/lang/reflect/Method;
  CORE        1  Ljava/util/concurrent/atomic/AtomicLong;
  CORE        1  Llibcore/io/AsynchronousCloseMonitor;
  CORE        1  Llibcore/io/Memory;
  CORE        1  Llibcore/util/NativeAllocationRegistry;
  CORE        1  Lsun/nio/ch/FileChannelImpl;
  ANDROID     1  Landroid/system/OsConstants;
  OTHER      12  Ljdk/internal/misc/Unsafe;

### activity_lower: 105 native classes, 612 native methods
  CORE       21  Ljava/lang/Class;
  CORE       13  Ljava/lang/Character;
  CORE       12  Ldalvik/system/VMRuntime;
  CORE       12  Ljava/util/zip/ZipFile;
  CORE       11  Ljava/lang/Math;
  CORE       10  Ljava/lang/String;
  CORE        9  Lcom/android/icu/util/regex/MatcherNative;
  CORE        9  Lsun/misc/Unsafe;
  CORE        6  Llibcore/math/NativeBN;
  CORE        5  Ljava/lang/System;
  CORE        5  Ljava/lang/Thread;
  CORE        4  Lcom/android/icu/util/regex/PatternNative;
  CORE        4  Ldalvik/system/DexFile;
  CORE        4  Ljava/lang/Object;
  CORE        4  Ljava/lang/reflect/Field;
  CORE        3  Ldalvik/system/VMStack;
  CORE        3  Llibcore/icu/ICU;
  CORE        3  Llibcore/util/CharsetUtils;
  CORE        2  Lcom/android/icu/charset/NativeConverter;
  CORE        2  Lcom/android/icu/util/CaseMapperNative;
  CORE        2  Ljava/io/ObjectInputStream;
  CORE        2  Ljava/lang/Double;
  CORE        2  Ljava/lang/Float;
  CORE        2  Ljava/lang/StringFactory;
  CORE        2  Ljava/lang/Throwable;
  CORE        2  Ljava/lang/UNIXProcess;
  CORE        2  Ljava/lang/VMClassLoader;
  CORE        2  Ljava/lang/ref/FinalizerReference;
  CORE        2  Ljava/lang/reflect/Array;
  CORE        2  Ljava/lang/reflect/Constructor;
  CORE        1  Ldalvik/system/BaseDexClassLoader;
  CORE        1  Ljava/io/FileDescriptor;
  CORE        1  Ljava/io/UnixFileSystem;
  CORE        1  Ljava/lang/Runtime;
  CORE        1  Ljava/lang/ref/Reference;
  CORE        1  Ljava/lang/reflect/Executable;
  CORE        1  Ljava/lang/reflect/Method;
  CORE        1  Ljava/util/concurrent/atomic/AtomicLong;
  CORE        1  Ljava/util/zip/Inflater;
  CORE        1  Llibcore/io/AsynchronousCloseMonitor;
  CORE        1  Llibcore/io/Memory;
  CORE        1  Llibcore/util/NativeAllocationRegistry;
  CORE        1  Lsun/nio/ch/FileChannelImpl;
  ANDROID    54  Landroid/graphics/RenderNode;
  ANDROID    34  Landroid/view/MotionEvent;
  ANDROID    27  Landroid/content/res/AssetManager;
  ANDROID    23  Landroid/graphics/Paint;
  ANDROID    17  Landroid/graphics/Canvas;
  ANDROID    17  Landroid/view/SurfaceControl;
  ANDROID    16  Landroid/graphics/HardwareRenderer;
  ANDROID    15  Landroid/graphics/Matrix;
  ANDROID    15  Landroid/os/Parcel;
  ANDROID    11  Landroid/graphics/Bitmap;
  ANDROID    10  Landroid/graphics/Region;
  ANDROID    10  Landroid/graphics/text/LineBreaker;
  ANDROID    10  Landroid/os/Trace;
  ANDROID     9  Landroid/graphics/BaseCanvas;
  ANDROID     9  Landroid/graphics/Path;
  ANDROID     9  Landroid/graphics/drawable/VectorDrawable;
  ANDROID     8  Landroid/graphics/Typeface;
  ANDROID     6  Landroid/graphics/BLASTBufferQueue;
  ANDROID     6  Landroid/os/Binder;
  ANDROID     6  Landroid/os/SystemProperties;
  ANDROID     6  Landroid/view/Surface;
  ANDROID     5  Landroid/content/res/ApkAssets;
  ANDROID     5  Landroid/content/res/XmlBlock;
  ANDROID     5  Landroid/graphics/ImageDecoder;
  ANDROID     5  Landroid/graphics/RecordingCanvas;
  ANDROID     5  Landroid/graphics/fonts/Font;
  ANDROID     5  Landroid/graphics/text/MeasuredText$Builder;
  ANDROID     5  Landroid/graphics/text/MeasuredText;
  ANDROID     5  Landroid/util/PathParser;
  ANDROID     4  Landroid/content/res/StringBlock;
  ANDROID     4  Landroid/graphics/fonts/Font$Builder;
  ANDROID     4  Landroid/graphics/fonts/FontFamily$Builder;
  ANDROID     4  Landroid/util/jar/StrictJarFile;
  ANDROID     3  Landroid/graphics/drawable/AnimatedImageDrawable;
  ANDROID     3  Landroid/graphics/text/PositionedGlyphs;
  ANDROID     3  Landroid/hardware/OverlayProperties;
  ANDROID     3  Landroid/os/SystemClock;
  ANDROID     3  Landroid/util/Log;
  ANDROID     3  Landroid/util/MemoryIntArray;
  ANDROID     3  Landroid/view/InputChannel;
  ANDROID     2  Landroid/graphics/ColorSpace$Rgb;
  ANDROID     2  Landroid/graphics/Interpolator;
  ANDROID     2  Landroid/graphics/NinePatch;
  ANDROID     2  Landroid/graphics/fonts/FontFamily;
  ANDROID     2  Landroid/graphics/text/TextRunShaper;
  ANDROID     2  Landroid/os/incremental/IncrementalManager;
  ANDROID     1  Landroid/graphics/ColorFilter;
  ANDROID     1  Landroid/graphics/DashPathEffect;
  ANDROID     1  Landroid/graphics/HardwareRendererObserver;
  ANDROID     1  Landroid/graphics/Shader;
  ANDROID     1  Landroid/graphics/drawable/AnimatedVectorDrawable;
  ANDROID     1  Landroid/os/MessageQueue;
  ANDROID     1  Landroid/os/Process;
  ANDROID     1  Landroid/system/OsConstants;
  ANDROID     1  Landroid/util/EventLog;
  ANDROID     1  Landroid/view/InputEventReceiver;
  ANDROID     1  Landroid/view/InputQueue;
  ANDROID     1  Landroid/view/SurfaceSession;
  OTHER      15  Ljdk/internal/misc/Unsafe;
  OTHER       1  Lcom/android/internal/content/om/OverlayConfig;
  OTHER       1  Lcom/android/internal/os/BinderInternal;
  OTHER       1  Lcom/android/internal/util/VirtualRefBasePtr;

### activity_upper: 300 native classes, 2270 native methods
  CORE      205  Lcom/android/org/conscrypt/NativeCrypto;
  CORE       64  Llibcore/io/Linux;
  CORE       32  Lsun/misc/Unsafe;
  CORE       31  Lsun/nio/fs/UnixNativeDispatcher;
  CORE       30  Ldalvik/system/VMRuntime;
  CORE       25  Ljava/lang/Class;
  CORE       19  Llibcore/io/Memory;
  CORE       17  Ljava/lang/Math;
  CORE       17  Lsun/nio/ch/Net;
  CORE       16  Ljava/util/zip/ZipFile;
  CORE       15  Lcom/android/icu/charset/NativeConverter;
  CORE       15  Ldalvik/system/VMDebug;
  CORE       14  Ljava/lang/reflect/Field;
  CORE       13  Ljava/lang/Character;
  CORE       13  Lsun/nio/ch/FileDispatcherImpl;
  CORE       11  Lcom/android/icu/util/regex/MatcherNative;
  CORE       11  Ljava/lang/String;
  CORE       11  Ljava/lang/Thread;
  CORE       10  Ldalvik/system/DexFile;
  CORE        8  Ljava/io/UnixFileSystem;
  CORE        8  Llibcore/icu/ICU;
  CORE        8  Lorg/apache/harmony/xml/ExpatParser;
  CORE        7  Llibcore/math/NativeBN;
  CORE        7  Lsun/nio/fs/LinuxNativeDispatcher;
  CORE        6  Ljava/lang/Runtime;
  CORE        6  Ljava/lang/System;
  CORE        6  Lorg/apache/harmony/xml/ExpatAttributes;
  CORE        5  Lcom/android/icu/util/LocaleNative;
  CORE        5  Ljava/lang/Object;
  CORE        5  Ljava/lang/reflect/Executable;
  CORE        5  Ljava/util/zip/Deflater;
  CORE        5  Lsun/nio/ch/EPoll;
  CORE        4  Lcom/android/i18n/timezone/internal/Memory;
  CORE        4  Lcom/android/icu/util/regex/PatternNative;
  CORE        4  Ljava/lang/StringFactory;
  CORE        4  Ljava/lang/UNIXProcess;
  CORE        4  Ljava/util/zip/Inflater;
  CORE        4  Llibcore/util/CharsetUtils;
  CORE        4  Lsun/nio/ch/DatagramDispatcher;
  CORE        4  Lsun/nio/ch/FileChannelImpl;
  CORE        4  Lsun/nio/fs/LinuxWatchService;
  CORE        3  Ldalvik/system/VMStack;
  CORE        3  Ljava/lang/reflect/Constructor;
  CORE        3  Ljava/util/prefs/FileSystemPreferences;
  CORE        3  Lsun/nio/ch/DatagramChannelImpl;
  CORE        3  Lsun/nio/ch/EPollPort;
  CORE        3  Lsun/nio/ch/IOUtil;
  CORE        2  Lcom/android/icu/util/CaseMapperNative;
  CORE        2  Ljava/io/FileDescriptor;
  CORE        2  Ljava/io/FileInputStream;
  CORE        2  Ljava/io/ObjectInputStream;
  CORE        2  Ljava/io/ObjectOutputStream;
  CORE        2  Ljava/lang/Double;
  CORE        2  Ljava/lang/Float;
  CORE        2  Ljava/lang/Throwable;
  CORE        2  Ljava/lang/VMClassLoader;
  CORE        2  Ljava/lang/ref/FinalizerReference;
  CORE        2  Ljava/lang/reflect/Array;
  CORE        2  Ljava/lang/reflect/Method;
  CORE        2  Ljava/util/zip/Adler32;
  CORE        2  Ljava/util/zip/CRC32;
  CORE        2  Lsun/nio/ch/NativeThread;
  CORE        2  Lsun/nio/ch/ServerSocketChannelImpl;
  CORE        2  Lsun/nio/ch/UnixAsynchronousServerSocketChannelImpl;
  CORE        1  Lcom/android/i18n/util/Log;
  CORE        1  Lcom/android/icu/text/TimeZoneNamesNative;
  CORE        1  Ldalvik/system/BaseDexClassLoader;
  CORE        1  Ljava/io/ObjectStreamClass;
  CORE        1  Ljava/lang/ref/Reference;
  CORE        1  Ljava/lang/reflect/Proxy;
  CORE        1  Ljava/net/SocketInputStream;
  CORE        1  Ljava/net/SocketOutputStream;
  CORE        1  Ljava/util/concurrent/atomic/AtomicLong;
  CORE        1  Ljava/util/jar/JarFile;
  CORE        1  Llibcore/io/AsynchronousCloseMonitor;
  CORE        1  Llibcore/util/NativeAllocationRegistry;
  CORE        1  Lorg/apache/harmony/dalvik/ddmc/DdmServer;
  CORE        1  Lorg/apache/harmony/dalvik/ddmc/DdmVmInternal;
  CORE        1  Lsun/nio/ch/PollArrayWrapper;
  CORE        1  Lsun/nio/ch/SocketChannelImpl;
  CORE        1  Lsun/nio/ch/UnixAsynchronousSocketChannelImpl;
  CORE        1  Lsun/nio/fs/UnixCopyFile;
  ANDROID    76  Landroid/graphics/RenderNode;
  ANDROID    53  Landroid/graphics/drawable/VectorDrawable;
  ANDROID    43  Landroid/graphics/Paint;
  ANDROID    43  Landroid/os/Parcel;
  ANDROID    43  Landroid/view/MotionEvent;
  ANDROID    42  Landroid/content/res/AssetManager;
  ANDROID    41  Landroid/view/SurfaceControl;
  ANDROID    35  Landroid/graphics/HardwareRenderer;
  ANDROID    33  Landroid/graphics/Bitmap;
  ANDROID    31  Landroid/media/MediaPlayer;
  ANDROID    28  Landroid/graphics/Canvas;
  ANDROID    25  Landroid/database/sqlite/SQLiteConnection;
  ANDROID    25  Landroid/graphics/Matrix;
  ANDROID    25  Landroid/graphics/Path;
  ANDROID    23  Landroid/filterfw/core/GLFrame;
  ANDROID    20  Landroid/database/CursorWindow;
  ANDROID    19  Landroid/content/res/XmlBlock;
  ANDROID    19  Landroid/graphics/BaseCanvas;
  ANDROID    19  Landroid/graphics/BaseRecordingCanvas;
  ANDROID    18  Landroid/app/NativeActivity;
  ANDROID    18  Landroid/graphics/Region;
  ANDROID    18  Landroid/media/MediaRecorder;
  ANDROID    18  Landroid/os/HwParcel;
  ANDROID    15  Landroid/filterfw/core/NativeFrame;
  ANDROID    15  Landroid/hardware/camera2/impl/CameraMetadataNative;
  ANDROID    15  Landroid/media/AudioTrack;
  ANDROID    14  Landroid/filterfw/core/ShaderProgram;
  ANDROID    14  Landroid/graphics/Typeface;
  ANDROID    14  Landroid/graphics/drawable/AnimatedVectorDrawable;
  ANDROID    13  Landroid/filterfw/core/NativeProgram;
  ANDROID    12  Landroid/animation/PropertyValuesHolder;
  ANDROID    12  Landroid/os/Binder;
  ANDROID    11  Landroid/filterfw/core/GLEnvironment;
  ANDROID    11  Landroid/graphics/ImageDecoder;
  ANDROID    11  Landroid/graphics/animation/NativeInterpolatorFactory;
  ANDROID    11  Landroid/graphics/animation/RenderNodeAnimator;
  ANDROID    11  Landroid/graphics/drawable/AnimatedImageDrawable;
  ANDROID    11  Landroid/graphics/fonts/Font;
  ANDROID    11  Landroid/media/MediaCodec;
  ANDROID    11  Landroid/os/Trace;
  ANDROID    11  Landroid/view/KeyCharacterMap;
  ANDROID    11  Landroid/view/Surface;
  ANDROID    10  Landroid/graphics/text/LineBreaker;
  ANDROID    10  Landroid/os/GraphicsEnvironment;
  ANDROID    10  Landroid/renderscript/RenderScript;
  ANDROID     9  Landroid/graphics/RecordingCanvas;
  ANDROID     8  Landroid/content/res/ApkAssets;
  ANDROID     8  Landroid/graphics/BLASTBufferQueue;
  ANDROID     8  Landroid/graphics/Picture;
  ANDROID     8  Landroid/hardware/Camera;
  ANDROID     8  Landroid/hardware/HardwareBuffer;
  ANDROID     8  Landroid/media/ImageWriter;
  ANDROID     8  Landroid/os/HwBlob;
  ANDROID     8  Landroid/os/SystemProperties;
  ANDROID     8  Landroid/util/PathParser;
  ANDROID     7  Landroid/graphics/SurfaceTexture;
  ANDROID     7  Landroid/media/ImageReader;
  ANDROID     7  Landroid/media/tv/tuner/Tuner;
  ANDROID     6  Landroid/filterfw/core/VertexFrame;
  ANDROID     6  Landroid/graphics/Gainmap;
  ANDROID     6  Landroid/graphics/GraphicsStatsService;
  ANDROID     6  Landroid/graphics/text/PositionedGlyphs;
  ANDROID     6  Landroid/media/AudioSystem;
  ANDROID     6  Landroid/media/ImageReader$SurfaceImage;
  ANDROID     6  Landroid/media/MediaMetadataRetriever;
  ANDROID     6  Landroid/os/MessageQueue;
  ANDROID     6  Landroid/view/InputChannel;
  ANDROID     6  Landroid/view/VelocityTracker;
  ANDROID     5  Landroid/graphics/BitmapFactory;
  ANDROID     5  Landroid/graphics/TextureLayer;
  ANDROID     5  Landroid/graphics/text/MeasuredText$Builder;
  ANDROID     5  Landroid/graphics/text/MeasuredText;
  ANDROID     5  Landroid/hardware/OverlayProperties;
  ANDROID     5  Landroid/hardware/SystemSensorManager;
  ANDROID     5  Landroid/hardware/camera2/utils/SurfaceUtils;
  ANDROID     5  Landroid/media/ImageWriter$WriterSurfaceImage;
  ANDROID     5  Landroid/media/MediaDrm;
  ANDROID     5  Landroid/media/audiofx/AudioEffect;
  ANDROID     5  Landroid/os/BinderProxy;
  ANDROID     5  Landroid/os/Debug;
  ANDROID     5  Landroid/os/HwRemoteBinder;
  ANDROID     5  Landroid/os/Process;
  ANDROID     5  Landroid/os/SystemClock;
  ANDROID     5  Landroid/util/jar/StrictJarFile;
  ANDROID     5  Landroid/window/ScreenCapture;
  ANDROID     4  Landroid/content/res/StringBlock;
  ANDROID     4  Landroid/drm/DrmManagerClient;
  ANDROID     4  Landroid/graphics/GraphicBuffer;
  ANDROID     4  Landroid/graphics/Interpolator;
  ANDROID     4  Landroid/graphics/NinePatch;
  ANDROID     4  Landroid/graphics/PathMeasure;
  ANDROID     4  Landroid/graphics/fonts/Font$Builder;
  ANDROID     4  Landroid/graphics/fonts/FontFamily$Builder;
  ANDROID     4  Landroid/hardware/SyncFence;
  ANDROID     4  Landroid/media/MediaSync;
  ANDROID     4  Landroid/net/LocalSocketImpl;
  ANDROID     4  Landroid/opengl/GLES20;
  ANDROID     4  Landroid/os/HwBinder;
  ANDROID     4  Landroid/util/MemoryIntArray;
  ANDROID     4  Landroid/view/InputQueue;
  ANDROID     3  Landroid/graphics/RuntimeShader;
  ANDROID     3  Landroid/media/AudioRecord;
  ANDROID     3  Landroid/media/CamcorderProfile;
  ANDROID     3  Landroid/media/PublicFormatUtils;
  ANDROID     3  Landroid/mtp/MtpServer;
  ANDROID     3  Landroid/net/rtp/AudioGroup;
  ANDROID     3  Landroid/os/FileObserver$ObserverThread;
  ANDROID     3  Landroid/os/UEventObserver;
  ANDROID     3  Landroid/util/EventLog;
  ANDROID     3  Landroid/util/Log;
  ANDROID     3  Landroid/view/DisplayEventReceiver;
  ANDROID     3  Landroid/view/InputEventReceiver;
  ANDROID     2  Landroid/app/ActivityThread;
  ANDROID     2  Landroid/app/backup/BackupDataInput;
  ANDROID     2  Landroid/graphics/BitmapShader;
  ANDROID     2  Landroid/graphics/CanvasProperty;
  ANDROID     2  Landroid/graphics/ColorSpace$Rgb;
  ANDROID     2  Landroid/graphics/HardwareRendererObserver;
  ANDROID     2  Landroid/graphics/PathIterator;
  ANDROID     2  Landroid/graphics/fonts/FontFamily;
  ANDROID     2  Landroid/graphics/pdf/PdfRenderer;
  ANDROID     2  Landroid/graphics/text/TextRunShaper;
  ANDROID     2  Landroid/hardware/camera2/DngCreator;
  ANDROID     2  Landroid/hardware/location/ActivityRecognitionHardware;
  ANDROID     2  Landroid/media/AudioPortEventHandler;
  ANDROID     2  Landroid/media/FaceDetector;
  ANDROID     2  Landroid/media/MediaCrypto;
  ANDROID     2  Landroid/media/MediaDescrambler;
  ANDROID     2  Landroid/media/MediaExtractor;
  ANDROID     2  Landroid/media/MediaHTTPConnection;
  ANDROID     2  Landroid/media/SoundPool;
  ANDROID     2  Landroid/media/audiofx/Visualizer;
  ANDROID     2  Landroid/media/audiopolicy/AudioVolumeGroupChangeHandler;
  ANDROID     2  Landroid/mtp/MtpDevice;
  ANDROID     2  Landroid/net/rtp/RtpStream;
  ANDROID     2  Landroid/os/PerformanceHintManager;
  ANDROID     2  Landroid/os/SharedMemory;
  ANDROID     2  Landroid/os/incremental/IncrementalManager;
  ANDROID     2  Landroid/util/CharsetUtils;
  ANDROID     2  Landroid/view/CompositionSamplingListener;
  ANDROID     2  Landroid/view/InputEventSender;
  ANDROID     2  Landroid/view/KeyEvent;
  ANDROID     2  Landroid/view/SurfaceSession;
  ANDROID     2  Landroid/view/TextureView;
  ANDROID     2  Landroid/view/TunnelModeEnabledListener;
  ANDROID     1  Landroid/app/Activity;
  ANDROID     1  Landroid/app/backup/BackupDataOutput;
  ANDROID     1  Landroid/app/backup/FileBackupHelperBase;
  ANDROID     1  Landroid/content/res/ResourceTimer;
  ANDROID     1  Landroid/database/sqlite/SQLiteGlobal;
  ANDROID     1  Landroid/graphics/BitmapRegionDecoder;
  ANDROID     1  Landroid/graphics/BlendModeColorFilter;
  ANDROID     1  Landroid/graphics/BlurMaskFilter;
  ANDROID     1  Landroid/graphics/Camera;
  ANDROID     1  Landroid/graphics/ColorFilter;
  ANDROID     1  Landroid/graphics/ColorMatrixColorFilter;
  ANDROID     1  Landroid/graphics/ComposeShader;
  ANDROID     1  Landroid/graphics/DashPathEffect;
  ANDROID     1  Landroid/graphics/DrawFilter;
  ANDROID     1  Landroid/graphics/HardwareBufferRenderer;
  ANDROID     1  Landroid/graphics/LightingColorFilter;
  ANDROID     1  Landroid/graphics/LinearGradient;
  ANDROID     1  Landroid/graphics/MaskFilter;
  ANDROID     1  Landroid/graphics/Movie;
  ANDROID     1  Landroid/graphics/PaintFlagsDrawFilter;
  ANDROID     1  Landroid/graphics/PathEffect;
  ANDROID     1  Landroid/graphics/PorterDuffColorFilter;
  ANDROID     1  Landroid/graphics/RadialGradient;
  ANDROID     1  Landroid/graphics/RegionIterator;
  ANDROID     1  Landroid/graphics/Shader;
  ANDROID     1  Landroid/graphics/SweepGradient;
  ANDROID     1  Landroid/graphics/pdf/PdfDocument;
  ANDROID     1  Landroid/graphics/pdf/PdfEditor;
  ANDROID     1  Landroid/graphics/text/GraphemeBreak;
  ANDROID     1  Landroid/hardware/SystemSensorManager$BaseEventQueue;
  ANDROID     1  Landroid/hardware/camera2/impl/CameraExtensionJpegProcessor;
  ANDROID     1  Landroid/hardware/usb/UsbDeviceConnection;
  ANDROID     1  Landroid/hardware/usb/UsbRequest;
  ANDROID     1  Landroid/media/JetPlayer;
  ANDROID     1  Landroid/media/MediaCodec$LinearBlock;
  ANDROID     1  Landroid/media/MediaMuxer;
  ANDROID     1  Landroid/media/RemoteDisplay;
  ANDROID     1  Landroid/media/ResampleInputStream;
  ANDROID     1  Landroid/media/ToneGenerator;
  ANDROID     1  Landroid/media/audiofx/SourceDefaultEffect;
  ANDROID     1  Landroid/media/audiofx/StreamDefaultEffect;
  ANDROID     1  Landroid/media/audiopolicy/AudioProductStrategy;
  ANDROID     1  Landroid/media/tv/tuner/Descrambler;
  ANDROID     1  Landroid/media/tv/tuner/Lnb;
  ANDROID     1  Landroid/media/tv/tuner/dvr/DvrPlayback;
  ANDROID     1  Landroid/media/tv/tuner/dvr/DvrRecorder;
  ANDROID     1  Landroid/media/tv/tuner/filter/Filter;
  ANDROID     1  Landroid/media/tv/tuner/filter/MediaEvent;
  ANDROID     1  Landroid/media/tv/tuner/filter/SharedFilter;
  ANDROID     1  Landroid/media/tv/tuner/filter/TimeFilter;
  ANDROID     1  Landroid/mtp/MtpDatabase;
  ANDROID     1  Landroid/os/HidlMemory;
  ANDROID     1  Landroid/os/MemoryFile;
  ANDROID     1  Landroid/os/ServiceManager;
  ANDROID     1  Landroid/system/OsConstants;
  ANDROID     1  Landroid/text/AndroidCharacter;
  ANDROID     1  Landroid/view/InputApplicationHandle;
  ANDROID     1  Landroid/view/InputWindowHandle;
  ANDROID     1  Landroid/webkit/WebViewLibraryLoader;
  OTHER      23  Ljdk/internal/misc/Unsafe;
  OTHER      14  Lcom/google/android/gles_jni/EGLImpl;
  OTHER       7  Lcom/android/internal/os/FuseAppLoop;
  OTHER       6  Lcom/android/internal/os/LongArrayMultiStateCounter;
  OTHER       5  Lcom/android/internal/os/LongMultiStateCounter;
  OTHER       2  Lcom/android/internal/os/BinderInternal;
  OTHER       2  Lcom/android/internal/os/LongArrayMultiStateCounter$LongArrayContainer;
  OTHER       2  Lcom/android/internal/util/VirtualRefBasePtr;
  OTHER       1  Lcom/android/internal/content/NativeLibraryHelper;
  OTHER       1  Lcom/android/internal/content/om/OverlayConfig;
  OTHER       1  Lcom/android/internal/os/ClassLoaderFactory;
  OTHER       1  Lcom/android/internal/os/ZygoteCommandBuffer;
  OTHER       1  Lcom/google/android/gles_jni/GLImpl;
  OTHER       1  Ljdk/internal/misc/VM;

```
