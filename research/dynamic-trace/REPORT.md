# Dynamic ART trace: the measured execution work-list

**Fidelity: `art-reference`.** Measured on real ART (Android 14 / API 34,
`google_apis` x86_64) running under a headless KVM emulator
(`acos-oracle-34`, 2048 MB, 4 cores, `-gpu swiftshader_indirect`).

This replaces the static reachability bounds in
`research/framework-dex/REPORT.md` (println 152–663 natives; minimal Activity
612–2,270 natives) with **measured dynamic facts**. Raw traces live in
`.cache/trace/` (gitignored); parsed machine-readable data is in
`research/dynamic-trace/data/`.

Everything is reproducible with `tools/oracle/trace-console.sh` and
`tools/oracle/trace-app.sh` (parsers: `art_trace.py`,
`binder_from_trace.py`, `timeline_from_trace.py`,
`compare_static_dynamic_natives.py`, `summarize_maps.py`).

## Method

- **Console path:** `dalvikvm64 -Xmethod-trace -Xmethod-trace-file:… \
  -Xmethod-trace-file-size:128M -verbose:jni -cp classes.dex Main`.
- **App path:** full instrumentation tracing (not sampling):
  `am start -S -W -P <file>` for launch-to-idle, and
  `am profile start <pkg> <file>` / `am profile stop <pkg>` around the
  interaction.
- The ART method-trace v3 binary format was decoded directly (14-byte records:
  `u16 tid`, `u32 method_id|exit_bit`, `u32 thread_cpu_usec`, `u32 wall_usec`;
  the method table is `*methods` … `*end`, with a `SLOW` marker when
  `clock=dual`). Every parsed trace passed a per-thread enter/exit balance check
  (a few unresolved frames are threads parked inside `Object.wait` at trace end).
- **Native classification** is exact: the full native-method set of the 12
  boot-classpath jars was extracted from the real DEX with the SDK `dexdump`
  (`data/native_methods_bootclasspath.tsv`, 5,313 methods; +conscrypt = the
  5,607 in the earlier report). The app's own natives are added from source
  (`data/native_methods_app_fixtures.tsv`). A traced method is native iff
  (class, method, signature) is in that set.
- Timestamps are the trace-relative wall clock (`wall_usec`); relative deltas
  are meaningful, absolute values are offset by the profiling enable point.
  Tracing adds large overhead, so times are for phase *ordering*, not
  performance (untraced `am start -W` numbers are given separately).

---

## Part A — console path (core library only)

| program | distinct methods | native methods | native classes | classes initialised (`<clinit>`) | method enters (excl. exits) |
|---|---:|---:|---:|---:|---:|
| `01_hello` (println) | 251 | **21** | 9 | **3** | 5,996 |
| `17_generics_collections` | 384 | **25** | 10 | 3 | 9,285 |
| `19_threads_sync` | 312 | **28** | 11 | 3 | 105,895 |
| **union** | — | **30** | — | — | — |

The static estimate for println was 152–663 natives; **the measured value is
21**, i.e. 7×–32× smaller. Of the 21, only 13 were in the static lower-bound
set (see comparison below).

### Native work-list, `01_hello` (`data/natives_println.tsv`)

| class | method | enters | first order |
|---|---|---:|---:|
| `java.lang.String` | `charAt(I)C` | 1969 | 36 |
| `java.lang.String` | `getCharsNoCheck(II[CI)V` | 32 | 146 |
| `java.lang.System` | `arraycopy(...)` | 25 | 120 |
| `java.lang.Thread` | `currentThread()` | 21 | 201 |
| `java.lang.ref.Reference` | `refersTo0(Object)Z` | 17 | 207 |
| `com.android.icu.charset.NativeConverter` | `encode(J[CI[BI[IZ)I` | 16 | 174 |
| `libcore.io.Linux` | `writeBytes(FileDescriptor,Object,II)I` | 16 | 211 |
| `java.lang.Object` | `wait(JI)V` | 7 | 236 |
| `com.android.icu.charset.NativeConverter` | `charsetForName(String)Charset` | 6 | 30 |
| `java.lang.System` | `currentTimeMillis()` | 4 | 233 |
| `com.android.icu.charset.NativeConverter` | `setCallbackEncode(JII[B)V` | 3 | 121 |
| `java.lang.Thread` | `interrupt0()` | 3 | 240 |
| `java.lang.Throwable` | `nativeFillInStackTrace()` | 3 | 246 |
| `NativeConverter` | `getNativeFinalizer/openConverter/getAveBytesPerChar/getMaxBytesPerChar` | 1 each | 27–85 |
| `java.lang.Object` | `internalClone()`, `notifyAll()` | 1 each | 88, 218 |
| `dalvik.system.VMRuntime` | `getNotifyNativeInterval()`, `stopHeapTaskProcessor()` | 1 each | 103, 230 |

All of them run **after** `Main.main`; no native method is invoked before
`main` for this program (`data/trace_println.json` → `natives_before_main: []`).

`19_threads_sync` adds the thread/JNI natives the println path never touches:
`Thread.nativeCreate`, `Thread.nativeGetStatus`, `Thread.sleep`,
`System.nanoTime`, `StringFactory.newStringFromBytes`,
`String.fillBytesLatin1`, `Array.createObjectArray`.

### Classes initialised

Only **3** `<clinit>` methods execute in the whole run:
`java.nio.charset.StandardCharsets`,
`com.android.icu.charset.NativeConverter`,
`java.lang.ref.Reference$SinkHolder` (identical for all three console
programs). Hundreds of classes are *executed* (251–384), but their static
initialisers do not run because their classes are already initialised inside
the boot image.

### Boot image / zygote preloading — measured and explained

ART does not initialise the core library from scratch on each run. The stock
`dalvikvm64` run uses `/system/framework/x86_64/boot.art`, whose classes are
pre-compiled **and pre-initialised** at image build time. Evidence:

- `/system/etc/preloaded-classes` (extracted to
  `.cache/boot/android-34/preloaded-classes`) lists **17,344** classes/
  array-descriptors.
- a run that executes 251–384 methods performs only **3** `<clinit>`s.
- isolating the boot image with `-Ximage:`/`-Xnoimage-dex2oat`/`-Xint`
  (`data/console_image_variants.txt`) changes the count only to 14/3/3 —
  the in-memory image is *also* pre-initialised, so the true from-scratch
  `<clinit>` sequence is **not observable** through the `dalvikvm` CLI on this
  image.

**Consequence for ACOS (no zygote, no boot image):** ACOS must run the
`<clinit>` sequence for every class it actually uses at first active use. From
the trace we can compute a lower bound: the classes whose methods execute and
which declare a `<clinit>` in the real boot DEX. For `01_hello` that is a few
dozen classes, not 3 — the difference is exactly what the boot image hid. The
executed-class sets are in `data/trace_*.json` (`classes_executed`), and the
class-init-capable classes can be joined against
`data/native_methods_bootclasspath.tsv` / the boot DEX.

`-verbose:class` was attempted but this `libart` has class-load VLOG compiled
out (`data/verbose_class_note.txt`: 0 `[Loaded]` lines). Class load/init order
is therefore derived from the method trace (`<clinit>` records) and
`-verbose:jni` registrations (947 native registrations during VM start).

---

## Part B — Activity path (`hello-views`, `hello-jni`)

### Headline numbers

| phase | trace | distinct methods | native methods | native classes | `<clinit>` after fork |
|---|---|---:|---:|---:|---:|
| launch → first frame (hello-views) | `-P` | **7,018** | **379** | **81** | 12 |
| interaction (tap, type, tap, back) | `am profile start/stop` | **5,631** | **327** | **71** | 4 |
| launch → first frame (hello-jni) | `-P` | **5,980** | **379** | **83** | 11 |

The static minimal-Activity lower bound was 612 natives; the measured launch
uses **379** (and **139** by the time `MainActivity.onCreate` is reached).

### Native split, launch (`data/natives_activity_launch.tsv`)

- **core library: 92** natives (java.*, dalvik.*, libcore.*, jdk.*)
- **`android.*`: 287** natives
- other: 0

Top classes by distinct natives invoked:

| class | natives | | class | natives |
|---|---:|---|---|---:|
| `android.graphics.HardwareRenderer` | 29 | | `android.graphics.Canvas` | 10 |
| `android.graphics.RenderNode` | 28 | | `libcore.io.Linux` | 9 |
| `android.os.Parcel` | 26 | | `java.lang.String` | 8 |
| `android.graphics.Paint` | 25 | | `android.graphics.Typeface` | 8 |
| `android.content.res.AssetManager` | 19 | | `android.os.Binder` | 7 |
| `android.graphics.drawable.VectorDrawable` | 15 | | `android.graphics.Bitmap` | 7 |
| `android.view.SurfaceControl` | 12 | | `android.os.SystemProperties` | 6 |
| `android.content.res.XmlBlock` | 11 | | `java.lang.Class` | 6 |
| `dalvik.system.VMRuntime` | 11 | | `android.graphics.ImageDecoder` | 6 |

Interaction adds **131** natives not used during launch; the top classes differ
markedly: `Paint` 38, `Parcel` 27, `MotionEvent` 16, `RenderNode` 16,
`SurfaceControl` 15, `Unsafe` 13, `LineBreaker` 8, `RenderNodeAnimator` 7.

### Bytecode executed, by package (launch)

| package | distinct methods | invocations |
|---|---:|---:|
| `java.lang` | 315 | 55,227 |
| `android.view` | 1,523 | 15,625 |
| `android.content` | 494 | 14,252 |
| `android.os` | 442 | 13,092 |
| `android.graphics` | 989 | 11,592 |
| `java.util` | 341 | 10,408 |
| `android.util` | 221 | 8,614 |
| `android.app` | 591 | 4,478 |
| `android.text` | 317 | 3,264 |
| `android.widget` | 433 | 2,224 |
| `com.android.internal` | 348 | 2,039 |
| `android.animation` | 238 | 1,637 |
| `dalvik.system` | 60 | 1,356 |

Category totals: **core** 1,029 methods / 74,329 calls; **android** 5,979
methods / 77,980 calls. So ~85 % of distinct methods are framework code, while
call volume is split about evenly.

### Zygote preloading the app relied on

Of the **1,128 classes executed** during the launch, **1,076 (95.4 %)** are in
the zygote `preloaded-classes` list; only **52** are loaded fresh after fork
(mostly `android.net.*`, anonymous `$$ExternalSyntheticLambda` classes, and
`DisplayManager`/`ConnectivityManager` helpers). Twelve classes run a
`<clinit>` after fork:
`android.net.ConnectivityManager`, `NetworkRequest($Builder)`,
`NetworkCapabilities`, `UidRange`, `NetworkCapabilitiesUtils`, `NetworkUtils`,
`SystemCertificateSource$NoPreloadHolder`, `DeviceConfig`,
`Animator$AnimatorCaller`, `SurfaceControlRegistry`.

ACOS has no zygote: all 1,128 classes (and the 17,344-class preload list as a
whole) must be loaded/initialised by ACOS itself, at least once before
snapshotting.

### Binder work-list, launch (`data/binder_calls_launch.tsv`)

**51 distinct outgoing transactions across 19 interfaces**, first at trace
order 3,767 (≈ order 1 of the app-visible system-service calls). Interfaces
(app → system_server), in first-call order:

`IDisplayManager`, `INetworkManagementService`, `IPackageManager`,
`IActivityManager`, `IConnectivityManager`, `IServiceManager`,
`IGameManagerService`, `IWindowManager`, `IActivityTaskManager`,
`IActivityClientController`, `IVirtualDeviceManager`, `IAccessibilityManager`,
`IInputMethodManager`, `IVoiceInteractionManagerService`, `IWindowSession`,
`IGraphicsStats`, `IContentCaptureManager`, `ITextServicesManager`,
`IInputManager`.

Notable first-frame calls: `IWindowManager.openSession` → `IWindowSession`,
`IWindowSession.addToDisplayAsUser` (creates the `InputChannel`),
`IWindowSession.relayout`, `IWindowSession.finishDrawing`,
`IActivityClientController.activityResumed/activityIdle`,
`IPackageManager.getApplicationInfo/getPackageInfo`,
`IActivityManager.finishAttachApplication/setRenderThread`.

**Incoming (system_server → app): 3 transports / 5 handler methods** during the
traced launch:

- `android.view.IWindow.onTransact` ×2 → `ViewRootImpl$W.insetsControlChanged`
- `android.app.IApplicationThread.onTransact` ×1 →
  `ActivityThread$ApplicationThread.scheduleEnterAnimationComplete`
- `com.android.internal.inputmethod.IInputMethodClient.onTransact` ×3 →
  `InputMethodManager$2.setActive/onBindMethod/reportFullscreenMode`

`bindApplication` itself fires **before** the profiler starts
(`handleBindApplication` is already in the trace at 27.7 ms), which is why only
one `IApplicationThread` transaction is visible.

### Binder work-list, interaction (`data/binder_calls_interaction.tsv`)

**30 outgoing transactions across 13 interfaces**;
**11 incoming transports** including the input/IME path:

- `IWindow.onTransact` ×13 → `ViewRootImpl$W.resized` ×7,
  `insetsControlChanged` ×4, `showInsets`/`hideInsets` ×1
- `IRemoteInputConnection.onTransact` ×15 →
  `RemoteInputConnectionImpl.getSurroundingText` ×8,
  `setComposingRegion` ×5, `clearMetaKeyStates`, `finishComposingText`
- `IApplicationThread.onTransact` ×2 → `requestAssistContextExtras`,
  `profilerControl`
- `IResultReceiver.onTransact` ×4 → `ResultReceiver$MyResultReceiver.send` ×2,
  `SyncResultReceiver.send` ×2
- `IAutoFillManagerClient`, `ITextServicesSessionListener` callbacks

Outgoing interaction interfaces include `IWindowSession`, `IInputMethodManager`,
`IInputMethodSession`, `IAutoFillManager`, `IContentService`, `IAudioService`,
`IPackageManager`, `IAndroidFuture`, `IImeTracker`, `ITextServicesManager`.

### Non-Binder channels (`data/channels_*.tsv`)

Method-trace evidence by channel (launch): `Surface/BLAST/graphics` 246 distinct
methods, `Binder` internals 155, `vsync/Choreographer` 60,
`SystemProperties` 14, `InputChannel` 13, socket/pipe 8, ashmem/memfd 6.
Interaction is dominated by `MotionEvent`/`InputEventReceiver` and
`RenderNodeAnimator`/`Choreographer`.

- **Input:** `InputChannel`/`InputEventReceiver` + `MotionEvent` (native).
- **vsync:** `Choreographer`, `DisplayEventReceiver`.
- **graphics buffers:** `SurfaceControl`, `BLASTBufferQueue`, `HardwareRenderer`,
  `RenderNode`, `sync_file` fds.
- **system properties:** `SystemProperties.native_get*`/`native_get_long`/
  `native_get_boolean`/`native_find` are native and their **argument values are
  not in the trace**, so property *names* are not captured (limitation).
- **files:** `/proc/<pid>/maps` and `/proc/<pid>/fd` snapshots
  (`data/memory.json` → `memory_map`, `fd_snapshot`).

### Launch timeline (trace-relative, µs after profiling enable)

| milestone | wall µs |
|---|---:|
| `ActivityThread.main` / `handleBindApplication` | 27,682 / 27,686 |
| `Application.onCreate` | 61,927 |
| `Activity.performCreate` / `MainActivity.onCreate` | 76,114 / 76,170 |
| `ActivityThread.handleResumeActivity` | 138,632 |
| `MainActivity.onResume` | 138,678 |
| `SurfaceControl.<init>` | 140,112 |
| `ViewRootImpl.setView` | 142,363 |
| `Choreographer.doFrame` / `performTraversals` | 152,843 / 152,930 |
| `performMeasure` | 161,684 |
| `performLayout` | 174,995 |
| `performDraw` | 179,078 |
| `ThreadedRenderer.draw` (first frame) | 201,432 |

Untraced `am start -W` (`data/memory.json` → `timings`): **cold TotalTime
p50 = 466 ms, p95 = 536 ms; warm p50 = 17 ms, p95 = 44 ms** (10 runs each;
WaitTime ≈ TotalTime + 3 ms). The previous capture's 414 ms cold start is the
same order; tracing overhead is not in these numbers.

### Memory at idle after first frame (`data/memory.json`)

| run | TOTAL PSS | TOTAL RSS |
|---|---:|---:|
| 1 | 30,303 KB (29.6 MiB) | 109,140 KB |
| 2 | 30,455 KB (29.7 MiB) | 109,148 KB |
| 3 | 30,508 KB (29.8 MiB) | 109,228 KB |

Virtual mapping sizes by kind (`memory_map`): apk/jar 80.5 MiB,
native `.so` 102.9 MiB, ashmem/memfd 192.7 MiB, dex/oat/art 16.4 MiB,
fonts 2.6 MiB; plus large anonymous *reservations* (cfi shadow, libwebview
reservation, scudo reserves ≈ 11.6 GiB of address space, not RSS).
FD snapshot: 118 fds — 36 system files, 29 anon_inode (`sync_file`),
23 `/dev`, 11 sockets, 6 ashmem/memfd, 2 pipes.

### hello-jni

Launch uses **5,980 methods / 379 natives**; its six own natives execute:
`throwFromJNI` ×2, `stringFromJNI`, `add`, `sumArray`, `callBack`,
`registeredEcho` ×1 each. Binder: 48 outgoing / 3 incoming.

---

## Static vs measured natives

`data/natives_compare_*.tsv` contains the full union with `predicted` /
`measured` / `enters` columns.

| corpus | static predicted | measured used | both | predicted-but-unused | used-but-not-predicted |
|---|---:|---:|---:|---:|---:|
| println (lower bound) | 152 | 21 | 13 | 139 | 8 |
| activity (lower bound) | 612 | 379 | 226 | 386 | 153 |

**Predicted-but-unused** (the static closure over-approximates hard): most of
`android.graphics.*` (153), `android.view.*` (53), plus many
`java.lang`/`dalvik`/`libcore` natives that are only reachable for code paths
this fixture never takes (crypto, zip, file I/O, networking).

**Used-but-not-predicted** (the static roots missed these): almost all of
`android.content.res.AssetManager`/`ApkAssets`/`StringBlock`/`XmlBlock`
(native asset/XML access, e.g. `XmlBlock.nativeNext` ×226,
`XmlBlock.nativeGetName` ×143), `PropertyValuesHolder.nCallFloatMethod` (animate
path), `Activity.getDlWarning`, `ActivityThread.nInitZygoteChildHeapProfiling`.
The static closure used a no-resource view path, so resource loading was
under-modelled; conversely it included expensive graphics/crypto natives the
minimal app never calls.

---

## Prioritised implementation order for ACOS

Derived from the phase traces; each list is the exact set in
`data/implementation_order.json`.

**(i) `System.out.println` on the real core library — 21 natives** (above).
The 13 that the static analysis also predicted are the "safe" starting set;
the 8 used-but-not-predicted (`NativeConverter.*`, `Reference.refersTo0`,
`Linux.writeBytes`, `VMRuntime.stopHeapTaskProcessor`) must be added. Classes
with `<clinit>` to run from scratch: `StandardCharsets`, `NativeConverter`,
`Reference$SinkHolder` plus every `<clinit>`-bearing class in the executed set.

**(ii) Reaching `Activity.onCreate` — 139 natives.** Dominated by:
`String.charAt`, `System.arraycopy`, `Object.identityHashCodeNative`,
`Reference.*`, `Trace.nativeGetEnabledTags`, the **`android.os.Parcel`** family
(26 natives), `content.res.StringBlock`/`AssetManager`/`ApkAssets`
(resource loading), `graphics.Typeface` (font setup),
`dalvik.system.VMRuntime` (allocation), `SystemProperties.native_get*`,
`BinderProxy.transactNative`. This is the minimum to get through
`ActivityThread` attach + `Application` creation + `Activity.performCreate`.

**(iii) First frame — 379 natives** (launch). Add the rendering stack:
`HardwareRenderer` (29), `RenderNode` (28), `Paint` (25),
`VectorDrawable` (15), `SurfaceControl` (12), `Canvas` (10), `Bitmap` (7),
`ImageDecoder` (6), `BLASTBufferQueue` (5), `Path` (5), plus
`Choreographer`/vsync and `IWindowSession.addToDisplayAsUser`/`relayout`/
`finishDrawing` for the surface lifecycle.

**(iv) Interaction — add 131 natives** (union 510): `MotionEvent` (16),
`LineBreaker` (8), `RenderNodeAnimator` (7), `Matrix`,
`RenderNode` dirty/invalidate paths, and the IME/input binder interfaces
(`IRemoteInputConnection`, `IInputMethodSession`, `IInputMethodManager`).

**System-service call order to stub** (from `binder_calls_launch.tsv`):
`IServiceManager.checkService` → `IDisplayManager` → `IPackageManager` →
`IActivityManager.finishAttachApplication` → `IWindowManager.openSession` →
`IWindowSession.addToDisplayAsUser` → `IWindowSession.relayout` →
`IActivityClientController.activityResumed` → `IWindowSession.finishDrawing` →
`IActivityClientController.activityIdle`. Incoming callbacks to support:
`IApplicationThread`, `IWindow`, `IInputMethodClient`, `IRemoteInputConnection`,
`IResultReceiver`.

---

## Limitations / not captured

1. **From-scratch class-init sequence is not observable.** ART always enters
   through a pre-initialised boot image (or an in-memory image that is also
   pre-initialised); `-Ximage:`/`-Xnoimage-dex2oat` do not disable it. The
   preloaded-classes list and the executed-class set bound what ACOS must do,
   but the exact `<clinit>` order is not measured.
2. **`-verbose:class` is compiled out** of this `libart` (0 `[Loaded]` lines),
   so class *load* order is inferred from executed methods / `<clinit>` /
   JNI registrations, not observed directly.
3. **Binder oneway/reply flags are not in the method trace**, and
   `am trace-ipc stop --dump-file` did not include the app process section on
   this build (the global dump contains 31 processes including `system`, but
   not the fixture app). Direction (app→system vs system→app) is reliable;
   oneway vs synchronous reply is not captured.
4. **System property names** are not captured (native args are not traced;
   bionic reads the property area without a syscall, so `strace` on the process
   would not show names either).
5. **`<clinit>` counts are lower bounds for ACOS**; the measured 3/12/4 are
   ART-with-image numbers.
6. **Tracing overhead** distorts absolute timings; use `am start -W` numbers.
7. **Native sets are complete for the boot classpath and the app DEX**;
   natives in dynamically loaded libraries not registered through these classes
   would be missed (none observed here).
8. **`native .so` mapping count (1,126) counts segments, not libraries**; each
   `.so` contributes several mappings. File-backed totals are in
   `data/memory.json` → `memory_map.by_kind`.

## Reproduction

```bash
tools/oracle/emulator.sh start
tools/oracle/trace-console.sh      # Part A -> data/trace_*.json, natives_*.tsv
tools/oracle/trace-app.sh          # Part B -> timeline.json, memory.json, binder_calls_*.tsv
tools/oracle/emulator.sh stop
```
