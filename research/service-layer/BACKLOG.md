# Service layer + Binder replacement — implementation backlog

Companion to `REPORT.md` (`services.tsv`, `natives_plan.tsv`). Tasks are sized
for **one subagent ≈ one day**, each with a **verifying test** that can fail.
Ordering is by dependency, not by importance; milestones (M1–M5) are the gates
from `docs/V1.md` and `research/dynamic-trace/REPORT.md`.

Conventions:

* `[owns]` = files/dirs the task may touch. Two tasks with the same `[owns]` must
  not run concurrently.
* `dep` = hard dependency. Everything in the same "wave" can run in parallel.
* Every native task ends with a **native-diff** run (T-11) once that exists; until
  then it ends with the stated unit test.
* Nothing here rewrites framework bytecode. All tasks are add-only except the
  marked `acos-vm` registry edits (a hot file: serialise them).

---

## 0. Crate and module boundaries (proposed)

| crate | status | contents | why separate |
|---|---|---|---|
| `acos-vm` | exists | interpreter, heap, class model, VM threads, native registry, **core-library natives** (`src/corelib/{lang,math,collections,thread,enum}.rs`) | core-library natives need privileged heap access (handle tables, object layout, GC barriers). Splitting them would force an unsafe internal API. |
| `acos-parcel` | **new** | `Parcel` binary format, `Binder`/`BinderProxy` handles, `BinderRegistry`, AIDL transaction tables, caller-identity stack, fd plumbing | no VM/Android dependencies except "a parcel is bytes + a handle table"; testable standalone with golden vectors; unblocks Binder, `SurfaceControl` transactions and `IContentProvider` in parallel. |
| `acos-framework-natives` | **new** | every non-core native: resources (`AssetManager`/`ApkAssets`/`StringBlock`/`XmlBlock`), graphics shims → `acos-render`, text/typeface, `view.input`, `view.surface`, `DisplayEventReceiver`, `SystemProperties`/`Log`/`Trace`/`SystemClock`/`MessageQueue`, `MemoryIntArray`, `GraphicsStats` | one registration surface for the "framework edge"; keeps `acos-vm` from growing 500 natives. |
| `acos-services` | **new** | the mini-ATMS / mini-WMS + compositor / mini-PMS / registry / input dispatcher / vsync generator / Settings provider, as **pure Rust state machines** behind traits | the services must be unit-testable without a VM; they only see typed structs and `acos-parcel` buffers. |
| `acos-runtime` | **new** (or a module in `acos-cli`) | boot sequence, epoll event loop, thread objects, snapshot/restore, frame handoff | the thing that owns `main()`; sequencing logic lives here, not in natives. |
| `acos-render` | exists (concurrent) | software rasteriser, RenderNode/Canvas/Paint, text shaping | owned by the render workstream; this backlog only *consumes* its API (see §8.3 of REPORT). |
| `acos-apk` | exists | ZIP/ARSC/AXML/manifest | consumers: `acos-services::pms`, `acos-framework-natives::res`. |
| `acos-dex` | exists | DEX parse + payload decoding | consumer: AIDL table generator (T-07), `<clinit>` closure (T-32). |
| `acos-jni` | later | JNI ABI (V1 §2) | not on the hello-views path. |

**Hot files (serialise):** `acos-vm/src/vm.rs` (native registry + thread setup),
`acos-vm/src/corelib/mod.rs`, `acos-vm/src/heap.rs` (object layout),
`acos-cli/src/main.rs` (wiring).

---

## Wave 0 — foundations (no VM execution needed, fully parallel)

### T-01 · `acos-parcel` skeleton + scalar/string codec
`[owns] crates/acos-parcel/src/{lib,parcel,error}.rs`
Implement the Rust `Parcel`: growable buffer, read position, 4-byte alignment,
`readInt/writeInt/long/float/double/byte`, `String8`/`String16`, `create/freeBuffer`,
`dataPosition/setDataPosition/dataSize/dataAvail/appendFrom`, `allowFds`,
`markForBinder` bookkeeping hook.
**Test:** golden vectors: (a) encode the AIDL sequence for
`IWindowSession.relayout`'s first 6 args, decode, re-encode → byte-identical;
(b) `dataPosition` after every op matches a hand-computed table; (c) an
`appendFrom` of a sub-range is byte-identical to a copy.
`dep:` none. **size:** 1 day.

### T-02 · `acos-parcel`: binder refs, interface tokens, fds
`[owns] crates/acos-parcel/src/{binder,token,fd}.rs`
`writeStrongBinder/readStrongBinder` via a `BinderId ↔ handle` table;
`writeInterfaceToken/enforceInterface` (throws on mismatch, exposes
`peek_interface_token()` for dispatch); `writeFileDescriptor/readFileDescriptor`/
`hasFileDescriptors` on real `memfd`s; `isForRpc` false.
**Test:** round-trip identity (`writeStrongBinder(b); readStrongBinder() == b`),
token mismatch → error, fd round-trip preserves content across a
`Parcel.recycle()`, `String8` header word `(len<<1)|1` verified against the
documented writer (**U8**: assert the exact bytes for ASCII/UTF-8/empty).
`dep:` T-01. **size:** 1 day.

### T-03 · `BinderRegistry` + caller identity stack
`[owns] crates/acos-parcel/src/registry.rs`
Stable `u64` handles for local objects and proxies; name→binder table for
`IServiceManager`; per-thread `{uid,pid}` stack with
`clearCallingIdentity/restoreCallingIdentity` tokens; death-recipient no-op
registry (`linkToDeath`→true, never fires).
**Test:** `checkService("activity").descriptor == "android.app.IActivityManager"`;
unknown name → `None`; identity push/pop is LIFO and thread-local (2 threads
interleaved).
`dep:` none. **size:** 0.5 day.

### T-04 · `acos-apk` → `AssetManager` asset source (read-only)
`[owns] crates/acos-framework-natives/src/res/{mod,assets}.rs`
Wrap `acos-apk`'s ZIP/ARSC/AXML into an `AssetSource` trait: open (by fd or mmap),
`getResourceValue(ident, density, TypedValue)`, `openNonAsset`, `openXmlAsset`,
`assetRead/assetLength/assetDestroy`, configuration set/update, `ApkAssets::load`.
No theme/style application yet.
**Test:** for `framework-res.apk`: `android:attr/colorBackground` resolves for
the default theme; `assets/images/android-logo-mask.png` reads with the correct
length/sha; `aapt2 dump resources` differential on 200 random attrs.
`dep:` none. **size:** 1–1.5 days.

### T-05 · AXML pull-parser native state machine
`[owns] crates/acos-framework-natives/src/res/xmlblock.rs`
`XmlBlock.nativeCreateParseState/nativeNext/nativeGetName/nativeGetAttributeCount/
nativeGetAttributeName/Resource/DataType/Data/nativeGetSourceResId/
nativeDestroy(ParseState)` + `StringBlock.nativeGetString/GetStyle/GetSize`.
**Test:** parse `framework-res.apk:res/layout/screen_simple.xml` and the app's
`activity_main.xml`; element/attr stream identical to `acos-apk::axml` and to
`aapt2 dump xmltree`; call count for the reference layout ≈ 226 `next()`
(REPORT §5.1).
`dep:` T-04. **size:** 1 day.

### T-06 · Theme/style resolution (`nativeApplyStyle`, `nativeResolveAttrs`, `nativeRetrieveAttributes`, `nativeTheme*`)
`[owns] crates/acos-framework-natives/src/res/theme.rs`
Theme bags, parent chains, `?attr` resolution, density-driven value selection,
`TypedArray` fill (the `int[]` layout that `TypedArray.obtain/resize` expects).
**Test:** resolve `android:style/Theme.DeviceDefault.Light` for the app and
assert 30 named attrs equal `aapt2 dump style`; assert `TypedArray.length()` and
`getInt/getFloat/getValueAt` on a `TextView` style match the reference values.
`dep:` T-04, T-05. **size:** 1.5 days.

### T-07 · AIDL table generator (`tools/gen-aidl-tables`)
`[owns] tools/gen-aidl-tables/`, `crates/acos-parcel/src/aidl_tables.rs` (generated)`
Decode every `*$Stub.onTransact` packed-switch from the boot DEX (via `acos-dex`)
into `(descriptor, code) → method/signature`, and the argument/reply layout from
the matching `*$Stub$Proxy.<method>` `write*/read*` sequence. Emit a Rust static
table for the ~110 methods in `services.tsv` (others lazily/unparsed).
**Test:** for `IWindowSession`, decoded codes match a hand-checked sample from
`dexdump -d .cache/boot/android-34/dex/framework/classes4.dex` (e.g.
`setOnBackInvokedCallbackInfo` is the code whose block reads
`OnBackInvokedCallbackInfo.CREATOR`); the table covers 100 % of the 83 outgoing
rows in `services.tsv`; regenerate → identical file (deterministic).
`dep:` none (needs `acos-dex` only). **size:** 1.5 days.

### T-08 · Parcelable codec helper ("materialise-and-write")
`[owns] crates/acos-parcel/src/parcelable.rs`, `crates/acos-runtime/src/javabridge.rs`
A trait for the reply builder: a Rust *value description* that (a) can be written
by a generated Rust codec, or (b) can be materialised as a Java object through the
VM and written by its own `writeToParcel`. Start with (b) only for `Rect`/
`Point`/`Size` as the smoke test, then `ApplicationInfo`.
**Test:** `Rect` written via (b) reads back in Java with the same `left/top/right/
bottom`; a deliberately wrong field order in a *test-only* generated codec must
be caught by the Java reader's `enforceNoDataAvail` (proves the guard works).
`dep:` T-01, VM object materialisation (T-20). **size:** 1 day.

---

## Wave 1 — core-library natives → **M1: println on the real core library**

REPORT §8.2 wave 1; `natives_plan.tsv` module `core.*`. These touch `acos-vm`.

### T-09 · String natives (hot path)
`[owns] crates/acos-vm/src/corelib/lang.rs` (`String` section only)
`charAt` (22,881 launch / 1,969 println), `getCharsNoCheck`, `fillBytesLatin1`,
`toCharArray`, `fastSubstring`, `compareTo`, `intern`, `doReplace`, plus
`StringFactory.{newStringFromBytes,newStringFromChars,newStringFromUtf8Bytes}`.
Represent `String` exactly like ART (byte[] + coder/compressed flag) so
`fillBytesLatin1` is a memcpy, not a transcode.
**Test:** `cargo test -p acos-vm string_natives_art_semantics` incl.
UTF-16 surrogate pairs, `intern()` identity, `compareTo` on
latin1-vs-utf16 mixed strings; then M1 diff vs `fixtures/expected-art/dexprogs`.
`dep:` T-08 not needed. **size:** 1 day.

### T-10 · Object/System/Math/Float/Double natives
`[owns] crates/acos-vm/src/corelib/{lang,math}.rs`
`Object.{identityHashCodeNative,internalClone,wait,notifyAll}`,
`System.{arraycopy,currentTimeMillis,nanoTime,mapLibraryName}`,
`Math.{ceil,floor,cos,sin,tan,sqrt}`, `Float/Double` bit ops.
`identityHashCodeNative` must be a **stable per-object id** (persisted in the
snapshot), not the handle.
**Test:** `arraycopy` overlap (same array, forward/backward), typed arrays,
`Object.wait/notify` wake ordering with a 2-thread test, `identityHashCode`
stability across GC + snapshot round-trip, `Math.ceil(-0.5) == -0.0` sign.
`dep:` none (same file family as T-09 → serialise). **size:** 1 day.

### T-11 · Native-diff harness (`tools/oracle/native-diff`)
`[owns] tools/oracle/native_diff.py`, `crates/acos-runtime/src/nativelog.rs`
Run a scenario with a per-native call recorder emitting the same TSV columns as
`natives_*.tsv`; diff (key, count, order-window) against the ART file; fail on
extra **and** missing natives.
**Test:** a replay of `println` produces 21 keys / 2,130 invocations matching
`natives_println.tsv` within ordering tolerance; giving it a deliberately
incomplete native set fails with the exact missing key.
`dep:` none (can be written before M1). **size:** 1 day.

### T-12 · Threads, Reference, NativeAllocationRegistry
`[owns] crates/acos-vm/src/corelib/thread.rs`, `crates/acos-vm/src/heap.rs`
`Thread.{nativeCreate,nativeGetStatus,isInterrupted,interrupted,interrupt0,yield}`,
`Object.wait` parking on host threads, `Reference.{refersTo0,getReferent}` with GC
interaction, `NativeAllocationRegistry.applyFreeFunction` (idempotent free).
**Test:** 100× spawn/join/interrupt, `Reference` cleared exactly once under
GC-stress, free function called once per handle even if the Java side finalises
twice.
`dep:` T-10 (same file family → serialise with T-09/T-10). **size:** 1.5 days.

### T-13 · Unsafe + VMRuntime + VM + VMDebug
`[owns] crates/acos-vm/src/corelib/vmrt.rs` (new), `crates/acos-vm/src/vm.rs` (registry only)
`jdk.internal.misc.Unsafe` (13 measured; get/put/volatile/CAS/park/unpark),
`VM.getNanoTimeAdjustment`, `VMRuntime.{newUnpaddedArray,newNonMovableArray,
addressOf,registerNativeAllocation,registerNativeFree,notifyNativeAllocationsInternal,
clampGrowthLimit,vmInstructionSet,registerAppInfo,getNotifyNativeInterval,
runHeapTasks,stopHeapTaskProcessor,setDedupeHiddenApiWarnings}`,
`VMDebug.{startMethodTracingFd,stopMethodTracing}`.
Arrays are pinned for `addressOf`; offsets come from the field layout table.
**Test:** a 1,000-op Unsafe CAS stress with 4 threads (`compareAndSetInt`,
`getReferenceVolatile`), offset stability after GC compaction (pinned arrays never
move), tracing produces an ART-v3-compatible record stream for a trivial program.
`dep:` T-10. **size:** 1.5 days.

### T-14 · Class/reflection natives
`[owns] crates/acos-vm/src/corelib/reflect.rs` (new)
`Class.{getNameNative,classForName,newInstance,getDeclaredMethodInternal,
getDeclaredConstructorInternal,getDeclaredFields,getPublicFieldRecursive,
getDeclaringClass,getEnclosingClass,isAnonymousClass,isDeclaredAnnotationPresent,
getComponentType}` (getComponentType/isInterface/isArray are bytecode but their
data comes from the mirror), `reflect.{Field.get,getNameInternal}`,
`reflect.Array.createObjectArray`, `reflect.{Constructor.newInstance0,Method.invoke}`,
`VMClassLoader.findLoadedClass`, `VMStack.getStackClass2`.
**Test:** the 20 dexprogs plus a reflection matrix (private field get, ctor
newInstance, generic signature absent), and the measured counts from the launch
trace (6 `getPublicFieldRecursive` → 6 hits, no misses logged).
`dep:` T-10. **size:** 1.5 days.

### T-15 · libcore.io.Linux + Memory + UnixFileSystem (posix layer)
`[owns] crates/acos-vm/src/corelib/io.rs` (new)
`open/read/write/writeBytes/close/mmap/munmap/stat/fstat/access/getuid/getpid/
gettid/setenv/android_fdsan_exchange_owner_tag`, `Memory.peek*/poke*`,
`UnixFileSystem.{getBooleanAttributes0,list0}`, `FileDescriptor.isSocket`,
`AsynchronousCloseMonitor.signalBlockedThreads`, `FileInputStream.available0`.
Every failure is an `ErrnoException` with the real errno (framework code branches
on `ENOENT` etc.).
**Test:** file read/write/mmap round-trip; `stat` on a nonexistent path throws
`FileNotFoundException` with errno 2; `peekIntNative` after `addressOf` on a
pinned array agrees with `Unsafe.getInt`.
`dep:` T-13. **size:** 1 day.

### T-16 · ICU converters + Character tables
`[owns] crates/acos-vm/src/corelib/icu.rs` (new)
`NativeConverter` (9 natives) for UTF-8/UTF-16LE/BE/Latin-1/US-ASCII with the full
CharmapEncoder/Decoder callback protocol (`setCallbackEncode/Decode`,
`getMaxBytesPerChar`, `getAveBytesPerChar`), `LocaleNative.setDefaultNative`,
`Character.{getTypeImpl,getDirectionalityImpl,isDigitImpl,isUpperCaseImpl,
isLowerCaseImpl}`, `CharsetUtils.toUtf8Bytes`.
Unknown charset → `UNSUPPORTED_CHARSET` (never a silent substitution table).
**Test:** the 20 dexprogs' string output; malformed-UTF-8 decode through
`CharsetDecoder` produces the same replacement sequence as
`fixtures/expected-art/dexprogs/18_string_format_misc`; converter reuse across
calls (state machine) is correct for a chunked decode.
`dep:` T-10. **size:** 1.5 days.

### T-17 · **M1 gate**: run all 20 dexprogs on the real core library
`[owns] tools/oracle/run-dexprogs-art.sh` + a new `acos verify --suite dexprogs`
path (with the CLI owner)
Wire boot (core-oj + core-libart + core-icu4j only) → `dalvik.system.DalvikMain`/
`Main.main` entry as ART does → 20 programs, normal + GC-stress.
**Test:** `acos verify --suite dexprogs --json` → 20/20 stdout+exit equal to
`fixtures/expected-art/dexprogs`, zero `unsupported`; native-diff vs
`natives_println.tsv`/`natives_17_*/19_*` passes. **This is M1.**
`dep:` T-09…T-16, T-11. **size:** 1 day.

---

## Wave 2 — Parcel/Binder transport + framework boot → **M2: Application + Activity.onCreate**

### T-18 · `Parcel` natives (27) on `acos-parcel`
`[owns] crates/acos-framework-natives/src/parcel.rs`
Register every `android.os.Parcel.native*` against the handle table; `Parcel.obtain`
pooling; `recycle`; `markForBinder` releasing the recorded binder refs.
**Test:** the framework's own `Parcel` self-tests plus: write a `Bundle` with
int/string/list/binder/fd, `appendFrom`, read back through the Java API,
`enforceNoDataAvail` passes; native-diff: 1,835 `nativeReadInt` at launch ± 5 %.
`dep:` T-01, T-02, T-08. **size:** 1 day.

### T-19 · Binder natives + transport (`transactNative`, `execTransact` plumbing)
`[owns] crates/acos-framework-natives/src/binder.rs`, `crates/acos-runtime/src/transport.rs`
`BinderProxy.transactNative` (the single outgoing native), `Binder.{getNativeBBinderHolder,
getCallingUid,getCallingPid,clearCallingIdentity,restoreCallingIdentity,
setThreadStrictModePolicy,flushPendingCommands}`, `BinderProxy.isBinderAlive`;
incoming delivery: build a Parcel, enqueue `BinderDelivery` on the target looper,
call `Binder.execTransact(code, dataPtr, replyPtr, flags)`.
**Test:** a fake service registered as "test.svc" with a hand-written AIDL pair
(real `$Stub`/`$Stub$Proxy` from the boot DEX, e.g. `IServiceManager`) round-trips
int/string/binder/fd arguments; oneway calls discard the reply and swallow
errors; a re-entrant call (service calls back into the app stub) works and its
delivery lands on the app looper thread (assert thread id).
`dep:` T-18, T-03, T-07. **size:** 2 days.

### T-20 · VM ↔ Rust call bridge (object materialisation + field set)
`[owns] crates/acos-runtime/src/javabridge.rs`
`new_instance(class)`, `set_field(name, value)` for primitives/strings/objects/
arrays, `call_method(obj, name, sig, args)`, `handle` ↔ `Object` conversion.
Used by reply construction (T-08) and `bindApplication` (T-24).
**Test:** build a `Rect`, an `ApplicationInfo` (20 fields) and a `Bundle` from
Rust, then read them back from Java bytecode and compare; a wrong field type
must be a hard error, not a coercion.
`dep:` T-14 (reflection model). **size:** 1.5 days.

### T-21 · `svc.registry` + `IServiceManager`
`[owns] crates/acos-services/src/registry.rs`
Name→binder table for the 26 interfaces in `services.tsv`; `checkService`/
`getService` return real `Binder` objects with the right descriptor; unknown →
null; `addService` → error unless ACOS itself calls.
**Test:** `ServiceManager.getService("window")` yields an `IWindowManager` whose
`asInterface` round-trips; a bogus name returns null and `SystemServiceRegistry`
takes its real failure path (asserted by exception type).
`dep:` T-19. **size:** 1 day.

### T-22 · `svc.pms`: ApplicationInfo / PackageInfo / features from the APK
`[owns] crates/acos-services/src/pms.rs`
Build `ApplicationInfo`/`PackageInfo`/`ActivityInfo` descriptions from
`acos-apk`'s manifest parser + the ACOS VFS layout (uid 10192, sourceDir,
dataDir, nativeLibraryDir, targetSdk 34, minSdk 26, theme, class name); the
feature table; `notifyDexLoad`; `getPackagesForUid`.
**Test:** golden-file test: the description for `hello-views.apk` equals a
committed expectation derived from `fixtures/reference/.../manifest.json` +
`display.txt` (uid 10192); Java-side read-back of every field via T-20.
`dep:` T-20. **size:** 1.5 days.

### T-23 · `svc.atms`: attach handshake + lifecycle transaction stream
`[owns] crates/acos-services/src/atms.rs`
Process record, `attachApplication`, `finishAttachApplication`,
`activityResumed/activityIdle/setRenderThread/getDisplayId/
setTaskDescription/reportSizeConfigurations/checkPermission/getContentProvider`,
`getActivityClientController`, and the `ClientTransaction` builder
(Launch/Resume/Pause/Stop/Destroy) delivered via `IApplicationThread`.
**Test:** a scripted state machine test (pure Rust) asserting transitions
`attached → bindApplication → launched → resumed → idle → paused → stopped →
destroyed`; then a live test: `Application.onCreate` runs **before**
`MainActivity.onCreate` and the order matches `lifecycle-summary.txt` for
create/start/resume. **This reaches M2.**
`dep:` T-21, T-22, T-19. **size:** 2 days.

### T-24 · `bindApplication` delivery
`[owns] crates/acos-services/src/bind_app.rs`
Build the Java `AppBindData` (ApplicationInfo, provider list, config,
CompatibilityInfo, app class, restricted backup dir) and invoke
`ActivityThread$ApplicationThread.bindApplication` via the bridge; assert it is
delivered on the main looper.
**Test:** `Application.onCreate` observed exactly once; the process's `LoadedApk`
has the expected sourceDir/dataDir/classLoader; a deliberate delivery on a
non-main thread must be rejected by the test.
`dep:` T-23, T-20. **size:** 1 day.

### T-25 · `core.os` natives: properties, clocks, log, trace, process
`[owns] crates/acos-framework-natives/src/os/{props,clock,log,trace}.rs`
`SystemProperties.{native_get,native_get_int,native_get_long,native_get_boolean,
native_find}` + `$Handle`, seeded from `.cache/boot/android-34/build.prop`;
`SystemClock.*`; `Log.println_native` with ART's exact
`"I/AcosFixture: msg"` format; `Trace.{nativeGetEnabledTags,nativeSetAppTracingAllowed}`;
`Process.setThreadPriority`; `EventLog.writeEvent`; `GraphicsEnvironment.*`.
**Test:** `Build.FINGERPRINT`/`SDK_INT`/`SUPPORTED_ABIS` match `build.prop`;
`logcat`-equivalent sink reproduces `fixtures/reference/.../logcat_initial.txt`
exactly; unknown property → caller's default.
`dep:` none (parallel with Wave 1). **size:** 1.5 days.

### T-26 · `os.looper`: MessageQueue on epoll
`[owns] crates/acos-framework-natives/src/looper.rs`, `crates/acos-runtime/src/eventloop.rs`
`MessageQueue.{nativeInit,nativePollOnce,nativeWake}` = one epoll set per process
`{mq wake fd, input fds, vsync timerfd, binder eventfd}`; `-1` timeout when no
message is due; no periodic wakeups.
**Test:** (a) a Looper with no messages blocks ≥ 5 s and the process consumes
**0** CPU ticks (`/proc/self/stat` utime+stime unchanged); (b) `postDelayed` fires
within ±2 ms of the deadline; (c) `nativeWake` from another thread wakes it;
(d) 10k post/run cycles leak no fds.
`dep:` T-19 (eventfd/delivery). **size:** 1.5 days.

### T-27 · Settings provider (`IContentProvider.call`) + `MemoryIntArray`
`[owns] crates/acos-services/src/provider.rs`, `crates/acos-framework-natives/src/os/memintarray.rs`
`GET_global/GET_secure/GET_system` (+ the `Secure`/`Global`/`System` namespaces),
`PUT_*`, the generation counter as a 4-byte memfd + `ParcelFileDescriptor` in the
reply Bundle, `IContentService.registerContentObserver` + `onChange` on real
change.
**Test:** `Settings.Global.getInt(cr, "animator_duration_scale", 1)` returns 1;
setting a value bumps the generation (observed through `MemoryIntArray.nativeGet`);
`registerContentObserver` fires exactly once per real change and never spuriously.
`dep:` T-18 (fd Parcel), T-21. **size:** 1.5 days.

---

## Wave 3 — window, surface, vsync, first frame → **M3**

### T-28 · `view.surface` natives (SurfaceControl/Surface/BLAST/SurfaceSession)
`[owns] crates/acos-framework-natives/src/surface.rs`, `crates/acos-services/src/compositor.rs`
SurfaceControl handles → layer records; transactions (create/apply/merge/clear,
set position/alpha/matrix/flags/transform-hint/frame-timeline-vsync);
`nativeWriteTransactionToParcel`/`nativeReadFromParcel`; `Surface.nativeIsValid`;
`BLASTBufferQueue.{nativeCreate,nativeUpdate,nativeGetSurface,
nativeMergeWithNextTransaction}`; `SurfaceSession.nativeCreate`.
**Test:** a transaction set→apply changes the layer state and increments the
compositor generation; Parcel round-trip of a transaction is byte-stable; a
SurfaceControl from `relayout` can be passed back into `nSyncAndDrawFrame`
without a stale-handle error.
`dep:` T-19, T-20. **size:** 2 days.

### T-29 · `svc.wms`: window state machine + `addToDisplayAsUser`/`relayout`
`[owns] crates/acos-services/src/wms.rs`
Window records (token, LayoutParams, frame, insets, visibility, focus);
`openSession`; `addToDisplayAsUser` (create `InputChannel` + `InsetsState` +
`InsetsSourceControl[]` + outFrame); `relayout` (frames/configuration/surface);
`setOnBackInvokedCallbackInfo`, `updateRequestedVisibleTypes`,
`onRectangleOnScreenRequested`; `IWindowManager.{useBLAST,hasNavigationBar,
isInTouchMode,getCurrentAnimatorScale,setInTouchMode}`.
**Test:** golden-value test: `addToDisplayAsUser` for hello-views writes
frame `[0,128][1080,2337]`, status-bar inset 128, nav inset 63, ime hidden —
matching `display.txt`; `relayout` twice with the same params returns the same
frame and a stable surface; `ViewRootImpl.mWinFrame` (read back from Java) equals
the golden value.
`dep:` T-28, T-23. **size:** 2 days.

### T-30 · `view.vsync`: DisplayEventReceiver + vsync generator
`[owns] crates/acos-framework-natives/src/vsync.rs`, `crates/acos-services/src/vsync.rs`
`nativeInit(WeakRef,WeakRef,MessageQueue,int,int,long)` (fd registered with the
looper), `nativeScheduleVsync`, `nativeGetLatestVsyncEventData` (VsyncEventData:
frame timestamps, interval, vsync id, frame timeline vsync); deadline heap +
`timerfd_settime(ABS)` armed only when scheduled; boot-anchored 60 Hz grid.
**Test:** `Choreographer.postFrameCallback` fires within one interval of the
expected deadline; 8.3 ms-jitter bound; after the callback chain ends, **no**
timer is armed (assert `timerfd` disarmed and 0 wakeups over 1 s);
`nativeGetLatestVsyncEventData` returns monotonically increasing vsync ids.
`dep:` T-26. **size:** 1.5 days.

### T-31 · Graphics natives → `acos-render` (canvas/paint/path/matrix/region/bitmap/node)
`[owns] crates/acos-framework-natives/src/gfx/{canvas,paint,path,matrix,region,bitmap,node}.rs`
One Rust handle per `jlong` in `RenderContext`; `Canvas`/`RecordingCanvas`/
`BaseRecordingCanvas`, `Paint` (43), `Path` (9), `Matrix` (6), `Region` (2),
`Bitmap` (12), `RenderNode` (32), `VirtualRefBasePtr` inc/dec strong,
`BlendModeColorFilter`/`PorterDuffColorFilter`, `NinePatch` chunk validation.
**Test:** a recorded display list for a `TextView` draws via `acos-render` and the
output PNG has the expected non-background pixel count; `RenderNode` property
get/set round-trips (all 30); stale handle → structured error, not a panic;
`Paint` text metrics agree with the `acos-render` shaping API within 0.01 px.
`dep:` T-19 (transactions) not required; needs `acos-render` API (concurrent).
**size:** 2–3 days (split across two agents by file).

### T-32 · `render.hwui`: HardwareRenderer state machine
`[owns] crates/acos-framework-natives/src/gfx/hwui.rs`, `crates/acos-runtime/src/renderthread.rs`
`nCreateRootRenderNode/nCreateProxy/nSetSurface(Control)/nSyncAndDrawFrame/
nNotifyFramePending/nAllocateBuffers/nSetName/nSetRtAnimationsEnabled/nPause/
nSetStopped/nIsDrawingEnabled/nGetRenderThreadTid/nSetFrameCallback/
nRegisterAnimatingRenderNode/nLoadSystemProperties/nSetProcessStatsBuffer` and
the observer (`HardwareRendererObserver.nCreateObserver/nGetNextBuffer`).
Render thread = a VM thread; frame completion wakes the looper.
**Test:** two consecutive frames: the second `nSyncAndDrawFrame` is accepted (no
"frame pending" stall), `nNotifyFramePending` → `Choreographer` requests another
frame, `nGetRenderThreadTid` equals the real render thread's id, and the
frame-timeline buffer receives one sample per frame.
`dep:` T-28, T-29, T-30, T-31. **size:** 2–3 days.

### T-33 · Text: Typeface + LineBreaker + MeasuredText
`[owns] crates/acos-framework-natives/src/text/{typeface,linebreaker}.rs`
`Typeface.{nativeCreateFromTypeface,nativeAddFontCollections,nativeReadTypefaces,
nativeRegisterGenericFamily,nativeSetDefault,nativeForceSetStaticFinalField,
nativeGetStyle,nativeGetWeight}` from `.cache/boot/android-34/fonts/fonts.xml`;
`LineBreaker.{nInit,nComputeLineBreaks,nGetLine*,nGetLineFlag}`;
`MeasuredText$Builder.{nInitBuilder,nAddStyleRun,nBuildMeasuredText,nFreeBuilder}`;
`Paint` text metric natives routed here.
**Test:** bounds for the 5 reference strings must equal
`ui_initial.json` **exactly** (`"Hello ACOS"` → `[42,317][231,368]`,
`"Enter your name"` → `[42,368][1038,486]`, `"clicks: 0"` → `[42,612][175,663]`);
line-break offsets for a 2-line string match a golden file; missing glyph falls
back to the same font family Android picks.
`dep:` T-31, `acos-render::text`. **size:** 2–3 days.

### T-34 · `svc.wms.finishDrawing` → frame handoff to acos-render + agent API
`[owns] crates/acos-services/src/frame.rs`, `crates/acos-runtime/src/frames.rs`
`finishDrawing` applies the transaction, takes the rendered buffer, assigns
`frame_seq`, cause id (linked to the action that dirtied it) and content hash,
signals waiters, schedules the frame-complete callback; `acos capture --fresh`
reads it.
**Test:** after launch, `frame_seq == 1` and the frame is non-black with a stable
hash for a static screen; a second capture with no input returns the *same*
`frame_seq` (never labelled fresh); a tap produces `frame_seq == 2` with a cause
id naming the tap.
`dep:` T-32, T-33. **size:** 1.5 days. **→ M3 gate: first frame drawn.**

### T-35 · **M3 gate**: hello-views first frame vs the reference capture
`[owns] tools/` (verify path), `results/frames/`
Run launch → capture; compare with
`fixtures/reference/hello-views/art-api34-x86_64/screenshot_initial.png`
(pixel diff with a stated tolerance) and with `ui_initial.json` (exact bounds).
**Test:** non-black frame, all 5 texts present with exact bounds, pixel diff
within tolerance, `frame_seq=1`, launch wall time recorded to
`EXPERIMENTS.md`.
`dep:` T-34. **size:** 1 day.

---

## Wave 4 — input, UI tree, interaction → **M4 / M5**

### T-36 · `view.input` natives + input dispatcher
`[owns] crates/acos-framework-natives/src/input/{motionevent,channel,receiver,sender,key}.rs`, `crates/acos-services/src/input.rs`
`MotionEvent` (16) over a Rust struct; `InputChannel.{nativeReadFromParcel,
nativeGetToken}` as socketpairs carrying `InputMessage` frames;
`InputEventReceiver.{nativeInit,nativeFinishInputEvent}`; `InputEventSender`;
`KeyEvent.nativeNextId`; `KeyCharacterMap.*`; injection API
(`acos action tap/type/back` → `InputMessage` with the right source/flags/
display id).
**Test:** inject a DOWN/UP at (540,427) → the `EditText` receives focus; a
`MotionEvent` round-trip through the channel preserves action/flags/coords within
1/1000 px; `nativeFinishInputEvent` is called for every dispatched event (no
leaks); native-diff: 16 `MotionEvent` natives hit.
`dep:` T-29, T-26. **size:** 2 days.

### T-37 · `IWindow` incoming callbacks (`resized`/`insetsControlChanged`/`showInsets`/`hideInsets`/focus/visibility)
`[owns] crates/acos-services/src/wms_callbacks.rs`
Build `ClientWindowFrames`/`InsetsState`/`InsetsSourceControl[]` parcels and
deliver on the UI looper in the measured order.
**Test:** the interaction scenario produces the same relative order as
`binder_calls_interaction.tsv` (`resized` before `insetsControlChanged` before
`showInsets`); `ViewRootImpl` handles each without an exception; a delivered
`resized` with a changed frame triggers exactly one `performTraversals`.
`dep:` T-29, T-19. **size:** 1.5 days.

### T-38 · IME stub + `IInputMethodClient`/`IRemoteInputConnection` delivery
`[owns] crates/acos-services/src/ime.rs`, `crates/acos-framework-natives/src/ime.rs`
`addClient`, `startInputOrWindowGainedFocus` → `NO_IME` (+ declared capability),
`show/hideSoftInput` + `ResultReceiver` answers, `IImeTracker` tokens/`onProgress`,
`IInputMethodSession.{viewClicked,updateSelection}`,
`getImeTrackerService`, `reportPerceptibleAsync`, `removeImeSurfaceFromWindowAsync`;
deliver `setActive`/`onBindMethod`/`reportFullscreenMode`,
`IRemoteInputConnection.*` on the app looper.
**Test:** typing does not hang (a 5 s watchdog fails the test); `showSoftInput`
returns false and still answers the receiver; the **declared divergence**
("IME not bound") appears in the session evidence; `onProgress` count within the
measured 22 ± 3.
`dep:` T-36, T-37. **size:** 2 days.

### T-39 · `svc.a11y` + `svc.autofill` + `svc.textservices` + `svc.audio` + `svc.futures` + `svc.gfxstats` + `svc.net` + `svc.display`
`[owns] crates/acos-services/src/misc/{a11y,autofill,textservices,audio,futures,gfxstats,net,display}.rs`
The 12 "L/M" interfaces from `services.tsv` (disabled/absent answers, always
answering receivers; `IGraphicsStats` memfd; `DisplayInfo`).
**Test:** each interface has a unit test asserting (a) the exact reply values from
`services.tsv`, (b) no callback is left unanswered (watchdog), (c) unknown inputs
produce a structured error, not a default success.
`dep:` T-21, T-22. **size:** 2 days.

### T-40 · UI tree extraction (`acos observe --tree --text --json`)
`[owns] crates/acos-runtime/src/tree.rs`, CLI
Walk the real view hierarchy (`ViewRootImpl.mView`) through the bridge and emit
class/id/text/bounds/clickable/enabled.
**Test:** output equals `fixtures/reference/.../ui_initial.json` for
`title/input/btn/result` (exact text, ids, bounds) and after click1
(`clicks: 1`), after click2, after recreate.
`dep:` T-36, T-35. **size:** 1.5 days. **→ M4 gate: input + UI tree.**

### T-41 · **M5 gate**: replay the interaction trace
`[owns] tools/`, `results/`
Scenario: tap(540,427) → type "World" → tap(157,549) → tap → rotate → back.
Compare with `fixtures/reference/.../{lifecycle-summary.txt, observed-texts-*,
ui_after_*.json, logcat_recreate.txt}` and native-diff against
`natives_activity_interaction.tsv` (327 keys).
**Test:** lifecycle sequence identical, `clicks: N` correct, state restored after
recreate, native-diff within the agreed tolerance (±10 % counts, order-window),
documented divergences (no IME, no sound) listed in the evidence.
`dep:` T-40, T-38, T-39, T-37. **size:** 2 days.

---

## Wave 5 — snapshot, reset, concurrency (ADR-0010 gate)

### T-42 · Snapshot format + core-only snapshot/restore
`[owns] crates/acos-runtime/src/snapshot/`
Serialise class metadata, statics, heap arena, interned strings, identity hashes,
roots, metadata (format version, fingerprint, property hash, locale, clock
anchor). Restore into a fresh VM. Restrict to the core-library class set first.
**Test:** 100 restore cycles run the 20 dexprogs with identical output; the
invariants of REPORT §6.3 are individual tests (no threads, no fds, determinism,
idempotence, roots, no garbage).
`dep:` T-17. **size:** 3 days.

### T-43 · Framework snapshot: preload set + `<clinit>` closure + re-seed hooks
`[owns] crates/acos-runtime/src/{boot,preload,reseed}.rs`
Compute the preload closure from `.cache/boot/android-34/preloaded-classes` ∩ the
measured executed set (plus DEX-derived `<clinit>` closure), run it in a
deterministic order validated against the trace `first_order` witness, then
snapshot; implement the re-seed list (pid/tid, RNG, clock, properties, locale,
resources, binder identity, fds).
**Test:** snapshot → restore → launch hello-views; assert (a) no `<clinit>` runs
after restore for snapshot classes, (b) the measured init count is reported and
recorded (closes GAP G-6), (c) cold start from snapshot beats the from-scratch
number by the expected margin, (d) two workers from one snapshot are independent.
`dep:` T-42, T-23…T-38. **size:** 3 days.

### T-44 · Reset + concurrent workers (N = 1,2,5,10,20)
`[owns] crates/acos-runtime/src/workers.rs`, `crates/acos-cli`
Copy-on-write restore per worker, `acos reset --scope app`, `acos destroy`.
**Test:** `docs/V1.md` 5.1: N workers pass with independent state (no leakage);
per-worker MiB reported at each N; reset-to-ready and worker-create-to-frame
timings recorded against `BENCHMARKS.md` targets.
`dep:` T-43. **size:** 2–3 days.

---

## Tooling / measurement tasks (parallel, unblock other tasks)

| id | task | dep | size |
|---|---|---|---|
| T-45 | `tools/oracle/prop-keys.py`: decode `const-string` at every `SystemProperties.get*`/`find` call site in the executed method set → closes **G-2** without an emulator | `acos-dex` | 1 day |
| T-46 | `tools/oracle/feature-audit.py` + a logging feature table → closes **G-3** | T-22 | 0.5 day |
| T-47 | ICU data probe (`adb` commands in REPORT §5.2 G-1) + extract `icudt*.dat` into `.cache/boot/android-34/icu/` → closes **G-1** | emulator (ADR-0015) | 0.5 day |
| T-48 | Thread inventory probe (`ps -T`, `/proc/<pid>/task`) → closes **G-5** | emulator | 0.5 day |
| T-49 | `tools/oracle/parcel-diff`: capture the *bytes* of every outgoing transaction on ART (needs a `wrap.<pkg>` + `LD_PRELOAD` shim or an ART agent) and diff against ACOS's parcel writer → strongest possible R-02 closure | emulator + NDK | 2 days |
| T-50 | Interpreter microbenchmark on the measured workload (`methods_hello_views_launch` opcode histogram → M units/s) → R-09 input | `acos-vm` | 1 day |

---

## Parallelisation plan (concurrent agents, no shared files)

```
wave 0  ┌ A: T-01,T-02,T-03 (acos-parcel: one agent, sequential inside crate)
        ├ B: T-04,T-05,T-06 (res)
        ├ C: T-07 (AIDL gen) — independent
        ├ D: T-11,T-45,T-46,T-50 (tooling) — independent
        └ E: T-25 (core.os natives) — touches only new files
wave 1  ┌ F: T-09,T-10,T-12,T-13 (acos-vm corelib — SERIAL, same files)
        ├ G: T-14 (reflect.rs, new file) — parallel with F after T-10
        ├ H: T-15 (io.rs) after T-13
        └ I: T-16 (icu.rs) parallel with F
        then T-17 (M1 gate) by the F owner
wave 2  ┌ J: T-18,T-19 (parcel+binder natives) — needs A
        ├ K: T-20 (javabridge) — needs G
        ├ L: T-21,T-22 (registry, pms) — needs J,K
        └ M: T-26,T-27 (looper, provider) — needs J
        then T-23,T-24 (atms; M2 gate) by L owner
wave 3  ┌ N: T-28,T-29 (surface + wms) — needs M2
        ├ O: T-30 (vsync) — needs T-26
        ├ P: T-31 (gfx natives) — needs acos-render API only (2 agents by file)
        └ Q: T-33 (text) — needs P
        then T-32 (hwui) → T-34 (frame handoff) → T-35 (M3 gate)
wave 4  ┌ R: T-36 (input) → T-40 (tree) → M4
        ├ S: T-37,T-38 (window callbacks, IME)
        └ T: T-39 (misc services)
        then T-41 (M5 gate)
wave 5  T-42 (snapshot) → T-43 (framework snapshot) → T-44 (workers)
```

**Conflict rules**

* `acos-vm/src/corelib/*` and `acos-vm/src/vm.rs`: one agent at a time (F).
* `crates/acos-framework-natives/src/` splits cleanly by subdirectory
  (`res/`, `gfx/`, `text/`, `view`, `input/`, `os/`) — different agents may work
  concurrently **only** if `lib.rs`'s registration list is appended by one owner
  per wave (or generated from the module list).
* `crates/acos-services/src/` splits by file (`pms.rs`, `atms.rs`, `wms.rs`,
  `compose.rs`, …) — safe in parallel.
* `crates/acos-render/` is owned by another workstream: file only **API requests**
  (as issues/TODO in `acos-framework-natives/src/gfx/mod.rs`), never edit it.
* `acos-cli/src/main.rs` is a single-writer file: one owner per wave.

---

## Milestone summary

| gate | tasks | evidence produced |
|---|---|---|
| **M1** println on real core library | T-09…T-17 | 20/20 dexprogs, native-diff vs `natives_println.tsv` |
| **M2** `Application`+`Activity.onCreate` | T-18…T-27 | lifecycle `onCreate→onStart→onResume`, `logcat_initial.txt` match |
| **M3** first frame in `acos-render` | T-28…T-35 | `frame_seq=1`, non-black frame, pixel diff, exact text bounds |
| **M4** input + UI tree | T-36, T-40 | `ui_initial.json` equality, tap → `clicks: 1` |
| **M5** interaction trace | T-37…T-41 | `lifecycle-summary.txt` + `ui_after_*` + native-diff (327) |
| (after V1) snapshot/reset | T-42…T-44 | ADR-0010 gates 1 & 2 |
