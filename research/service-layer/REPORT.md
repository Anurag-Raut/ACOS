# The Rust service layer and the Binder replacement — implementation-ready design

**Status:** design + work-list (research artefact). No runtime code in this
directory. Everything here is derived from the measured ART traces in
`research/dynamic-trace/` and the artefacts in `.cache/boot/android-34/`.

**Fidelity labels used throughout**

| label | meaning |
|---|---|
| **MEASURED** | read out of `research/dynamic-trace/data/*` or a file in `.cache/boot/android-34/`; reproducible with the commands in §12 |
| **DERIVED** | computed from measured data by a script in this directory (`gen_natives_plan.py`, `gen_services.py`, `dex_method_sizes.py`) |
| **INFERRED** | reasoned from AOSP/API-34 semantics or from `~/Android/Sdk/sources/android-36` (API 34 sources are **not** installed) — needs verification |
| **GAP** | we do not know, and a specific measurement is proposed instead of guessing |

---

## 1. Verdict

**Recommendation.** Keep the framework's real Binder/AIDL code paths and replace
only their *transport*. ACOS executes the real `*$Stub$Proxy.transact` →
`BinderProxy.transactNative` → (in-process) Rust dispatcher → service handler
chain for the 68+5 transactions it makes at launch, and the reverse
`Binder.execTransact` → `*$Stub.onTransact` → handler chain for the 8 incoming
transports, with a **single** new native (`BinderProxy.transactNative`) and **no
new native at all in the incoming direction** (Android 14's `Binder.execTransact`
is Java bytecode that calls `execTransactInternal`; the JNI entry is only the
kernel's entry point, which ACOS does not have). Arguments are marshalled in the
real **Android Parcel** format, which is unavoidable anyway because the incoming
`onTransact` handlers parse Parcels, and because `SurfaceControl`, `InputChannel`,
`MemoryIntArray` and `ParcelFileDescriptor` traffic rides the same Parcel
primitives; the cost is irrelevant at the measured volume (73/106
`transactNative` calls, ~3.4k Parcel native calls per launch out of 34,262 native
invocations). Reply *content* is produced by **materialising the real framework
parcelable as a Java object and letting its own `writeToParcel` emit the bytes**
wherever the type is complex (`ApplicationInfo`, `PackageInfo`, `DisplayInfo`,
`ClientWindowFrames`, `InsetsState`), which removes the entire class of
"hand-written Parcel layout drift" bugs; small scalar replies are written
directly by Rust codecs. Services are grouped into seven in-process Rust
components (**registry, display, PMS, ATMS, WMS/compositor, input dispatcher +
IME stub, vsync/time**) that are state machines only where the framework makes
them observable (`IWindowSession.relayout`/`finishDrawing`, the lifecycle
transaction stream, IME tracking, the Settings provider generation counter).
Zygote is replaced by a **single deterministic boot snapshot** (ADR-0010) built
from the DEX-declared `<clinit>` closure of the reported executed-class set plus
the `preloaded-classes` list, with an explicit list of what must be excluded and
re-seeded.

**Why not the alternative (register Java `IInterface` implementations that bypass
`transact`).** It does not remove work, it moves it: (i) the *incoming* direction
still needs Parcel marshalling, because the parse side is the framework's own
`ViewRootImpl$W.onTransact` bytecode; (ii) it requires ~26 AIDL interfaces with
~110 methods implemented as **Java-level** classes — either hand-written DEX or
Rust-backed synthetic classes — plus every nested parcelable
(`WindowManager.LayoutParams`, `InsetsState`, `MergedConfiguration`,
`ClientWindowFrames`, `TaskDescription`, `InputBindResult`, …), i.e. it
re-implements the framework surface that ADR-0007 exists to avoid; (iii) the
saving is at most a few tens of microseconds per call at 51–106 calls, on a
budget measured at 466 ms cold / ~180 ms to first draw. It is a legitimate
*optimisation* later (ADR-0007 explicitly allows measured-and-verified
replacement), and the design below keeps that door open by putting the whole
dispatcher behind one trait (`ServiceDispatch`) with an optional local-interface
fast path (`queryLocalInterface` returning a Rust-backed object) that can be
switched on per interface and differential-tested.

**What this costs.** ~520 distinct native methods (union of launch, interaction
and println), 100 of them rated L; 26 outgoing interfaces (83 method rows) and 11 incoming
interfaces (25 rows) to answer; one snapshot format; and a real (if small) window/input/
vsync state machine. The DEX-side work is zero: no framework bytecode is
rewritten, generated or patched.

---

## 2. Headline counts (all MEASURED unless marked)

| fact | launch → first frame | interaction (tap,type,tap,back) | println |
|---|---:|---:|---:|
| distinct methods executed | 7,018 | 5,631 | 251 |
| **method invocations** (Σ `enters`) | **152,321** | **290,799** | 5,996 |
| distinct native methods | **379** | **327** | **21** |
| native invocations | 34,262 | 33,390 | 2,130 |
| estimated interpreted 16-bit code units (DERIVED, §9.1) | 3,182,748 | 7,061,529 | 95,317 |
| avg units per invocation (DERIVED) | 20.9 | 24.3 | 15.9 |
| classes executed | 1,128 | — | — |
| classes initialised after fork (`<clinit>`) | 12 | 4 | 3 |
| outgoing transactions (`transactNative`) | **73** | **106** | 0 |
| — distinct AIDL (interface, method) pairs | 51 | 30 | 0 |
| — distinct interfaces | **19** | **13** | 0 |
| — hand-written `IContentProvider.call` (not in the TSV, see §3.7) | 5 | 4 | 0 |
| incoming transports / handler rows | 3 / 5 | 6 / 14 | 0 |
| wall clock, untraced `am start -W` | cold p50 **466 ms**, p95 536; warm p50 17 | — | — |
| PSS / RSS at idle | 30.3 MB / 109 MB, 118 fds | — | — |

**Native union across all three traces: 520 distinct natives in 96 classes**
(DERIVED from the three `natives_*.tsv` files). Of the 510 activity union, 131
are interaction-only; println contributes 10 natives the activity traces never
touch.

**Native classes by count (MEASURED):** `Paint` 43, `RenderNode` 30,
`HardwareRenderer` 30, `Parcel` 27, `AssetManager` 21, `SurfaceControl` 21,
`MotionEvent` 16, `VectorDrawable` 15, `VMRuntime` 13, `Unsafe` 13, `Bitmap` 12,
`XmlBlock` 11, `Class` 11, `libcore.io.Linux` 10, `Canvas` 10, `Path` 9,
`NativeConverter` 9, `String` 8, `Thread` 8, `Typeface` 8, `LineBreaker` 8.

**Interpretation of the counts.** Execution *volume* is dominated by a handful
of core natives (`String.charAt` 22,881, `System.arraycopy` 964,
`Reference.refersTo0` 486, `String.fillBytesLatin1` 243), while *work* is
dominated by breadth in `android.graphics`/`android.view`/resources. Binder is
2–3 % of the calls (73 transactions) and ~10 % of native invocations (3,391
launch) — so **Binder fidelity is cheap and graphics fidelity is expensive**,
which is the opposite of what a "replace system_server" narrative usually
predicts.

---

## 3. Binder replacement architecture

### 3.1 The two options, evaluated against the measured volume

| criterion | (a) faithful `transact` + Parcel | (b) registered Java `IInterface` implementations |
|---|---|---|
| new natives needed | 1 (`BinderProxy.transactNative`) + Parcel natives (needed anyway) | Parcel natives **still needed** for incoming `onTransact` |
| Java-level surface to write | **none** (real proxies/stubs) | 26 interfaces / ~110 methods + ~30 parcelable types, as DEX or synthetic classes |
| marshalling cost | 2 PWs, ~3.4k Parcel native calls/launch | 0 for outgoing, unchanged for incoming |
| failure mode | wrong parcel layout → exception or wrong value (loud, testable) | wrong method semantics → silently different behaviour |
| reflection/`asInterface` behaviour | identical to Android (real `BinderProxy`, descriptor, `queryLocalInterface` returns null) | must fake `IBinder`/descriptor identity; framework code that compares binders can observe the difference |
| deadlock/hang risk | none new (in-process, synchronous) | same |
| upgrade to API 35 | free (real DEX drives everything) | re-implement the delta |
| measurable payoff | — | ≤ 106 × (marshalling ≈ few µs) ≈ < 1 ms total |

**Decision: (a).** The single quantitative argument is that the marshalling cost
is bounded by 106 calls and is not on any critical path in the measured timeline
(`relayout` at 120 ms, `finishDrawing` at 135 ms of a 466 ms cold start, with the
frame itself produced by the renderer, not by the IPC).

### 3.2 The transport

```
outgoing (app -> system), one native:
  android.os.BinderProxy.transactNative(code:int, data:Parcel, reply:Parcel, flags:int)Z
      -> Rust: despatch(descriptor = data.peek_interface_token(),
                        code, data_cursor, reply_writer, flags) -> ServiceReply
      -> reply Parcel bytes (Android format) -> Java reads them, unaware a syscall never happened

incoming (system -> app), zero new natives:
  Rust transport builds a Parcel (Parcel.obtain() -> nativeCreate handle, write args)
      -> enqueue BinderDelivery into the target Looper's MessageQueue
      -> target thread: binder.execTransact(code, data.mNativePtr, reply.mNativePtr, flags)
         (Java bytecode: Parcel.obtain(dataObj) -> execTransactInternal -> onTransact)
      -> *$Stub.onTransact -> real handler
```

Justification for the incoming shape: **MEASURED** `android.os.Binder.execTransact
(IJJI)Z` and `execTransactInternal(ILandroid/os/Parcel;Landroid/os/Parcel;II)Z`
appear in the method trace as **bytecode**, not natives, and no
`Binder.execTransact` entry exists in the 379-name native list. Android 14 moved
the JNI entry point out of the Java method; ACOS therefore inherits the whole
Java-side incoming path for free and only has to *call* it.

### 3.3 Parcel semantics ACOS must implement

The 27 measured `Parcel` natives (MEASURED; counts from launch) define the exact
surface. Implement a Rust `Parcel` with a growable buffer + read position, and
match Android's binary layout because **the framework's own Java codecs are the
other end**:

| group | natives | notes |
|---|---|---|
| scalars | `nativeReadInt` (1,835), `nativeWriteInt` (289), `nativeReadLong` (23), `nativeWriteLong` (11), `nativeReadFloat` (43), `nativeWriteFloat` (19) | 4-byte aligned little-endian; writes are aligned to 4 bytes including after a string |
| strings | `nativeReadString16` (173), `nativeWriteString16` (68), `nativeReadString8` (178), `nativeWriteString8` (9) | `String16` = u32 length (incl. NUL) + UTF-16LE + NUL + pad(4); `String8` = u32 byte length + UTF-8 + NUL (and Android 14 writes a 5-byte "header" word: `(len<<1)|1`, truncated for ASCII) — **verify against the real writer, see test T-3.3** |
| buffer | `nativeCreate`, `nativeFreeBuffer` (141), `nativeDataPosition` (86), `nativeSetDataPosition` (59), `nativeDataSize`, `nativeDataAvail` (11), `nativeAppendFrom` (6) | position semantics are observable by framework code; `obtain()` pools buffers |
| binder refs | `nativeMarkForBinder` (67), `nativeWriteStrongBinder` (37), `nativeReadStrongBinder` (16) | the marker records which objects were written so `Parcel.recycle` can release them; a strong binder is written as a 4-byte handle into a parallel table (real Android uses `flat_binder_object`); ACOS writes a u32 id and keeps a Rust `BinderId -> Object` map |
| interface token | `nativeWriteInterfaceToken` (73), `nativeEnforceInterface` (6) | token = string16 descriptor; `enforceInterface` throws `SecurityException` on mismatch — implement it faithfully, it also gives the dispatcher its descriptor |
| file descriptors | `nativeReadFileDescriptor` (6), `nativeHasFileDescriptors` (6) | **must hand out real fds** (Settings generation counter, `IGraphicsStats`); use `memfd_create` and a per-Parcel fd list |
| flags/fds | `nativePushAllowFds` (5), `nativeRestoreAllowFds` (5), `nativeIsForRpc` (6) | RPC = false always |
| misc | `nativeSetDataPosition`, `nativeCreate` | — |

Two natives are *not* Parcel but ride the same "opaque jlong handle" pattern and
must share its registry: `android.util.MemoryIntArray` (`nativeOpen`/`nativeSize`/
`nativeGet`) and `com.android.internal.util.VirtualRefBasePtr`
(`nIncStrong`/`nDecStrong`).

### 3.4 IBinder identity, death recipients, oneway

* **Identity.** Every service binder and every app-side binder gets a stable
  `u64` handle from one registry (`BinderRegistry`). `BinderProxy.transactNative`
  receives the proxy object; its `mNativePtr` (a `BinderProxy` field set by
  `BinderProxy.getInstance`) is the handle, so no map lookup by object identity is
  needed. `Binder.getNativeBBinderHolder()` (18 calls at launch) must return a
  handle from the *same* namespace so `==`/`equals`/`hashCode` and
  `Binder.isProxy()` behave exactly as on Android, and so `Parcel.writeStrongBinder`
  round-trips (`writeStrongBinder(b) then readStrongBinder() == b`).
* **Death.** Nothing dies in-process, so `linkToDeath` must **succeed** (return
  true to `IBinder.linkToDeath`) and **never** call `binderDied`; `unlinkToDeath`
  returns true; `isBinderAlive` returns true; `pingBinder` true. Death is a
  documented no-op capability, not a silent stub: if a scenario kills a service,
  ACOS must instead make the dependent call return a structured
  `UNSUPPORTED_SERVICE` error. (No `linkToDeath` calls appear in the measured
  native list; flagging it because third-party code does it routinely.)
* **Oneway.** `flags & 1`. Outgoing oneway: execute inline (the Rust handler is
  cheap and has no Java callback), but **discard the reply parcel** and swallow
  handler errors exactly as Binder does (`RemoteException` is not thrown to a
  oneway caller). Incoming oneway: **enqueue on the target looper** — never
  invoke the app's stub re-entrantly from the caller's frame; real Binder hands
  off to a binder thread and the stub posts to a handler, and the measured
  handlers (`ViewRootImpl$W.*`, `InputMethodManager$2.*`) all assume that.
* **Caller identity.** `Binder.getCallingUid()` (29)/`getCallingPid()` (21) are
  served from a per-thread Rust frame pushed by the dispatcher
  (`{uid, pid}` = the app's own 10192 / its pid for re-entrant calls; = 1000 /
  `system_server`'s synthetic pid for framework→app deliveries).
  `clearCallingIdentity` (26)/`restoreCallingIdentity` (1) push/pop a token; the
  token is the packed (uid,pid) pair. `setThreadStrictModePolicy` (4) stores a
  per-thread int; `flushPendingCommands` (4) is a no-op.
* **ServiceManager.** `ServiceManager.getService`/`checkService` (9 at launch, 1
  in interaction) reaches `IServiceManager$Stub$Proxy.checkService`. ACOS answers
  from the `BinderRegistry`, and the returned binder's descriptor is the requested
  name so `IInterface.asInterface` produces the right real stub/proxy. Crucially
  **unknown names return null** — the framework then takes its real "service is
  missing" path (`SystemServiceRegistry` throws or degrades), which is the honest
  behaviour ADR-0008 requires.

### 3.5 Transaction codes: derive them, do not hand-write them

AIDL codes are not guessable, but they are **statically extractable from the real
boot DEX**: every `*$Stub.onTransact` starts with
`enforceInterface(DESCRIPTOR)` and then a `packed-switch` on `code` whose targets
are the per-method blocks. Sketch, verified by hand on
`android.view.IWindowSession$Stub.onTransact`
(`.cache/boot/android-34/dex/framework/classes4.dex`): the switch is
`packed-switch v14, 00000792` / `packed-switch v14, 00000798` and the payloads are
`packed-switch-data` pseudo-instructions directly after the method. A generator
(`tools/gen-aidl-tables`, to be written — backlog T-07) decodes:

* `(descriptor, code) -> method name + signature` (from `onTransact`),
* the **argument layout** in the same order (from the `$Stub$Proxy.<method>`
  bytecode: the sequence of `Parcel.write*` calls before `transact`),
* the **reply layout** (from the same proxy: the `Parcel.read*` calls after
  `transact`).

`acos-dex` already decodes payloads (it must, to interpret switches), so the
generator reuses it. The generated table is data, not code: a Rust
`&'static [AidlMethod]` keyed by (descriptor, code), used for dispatch, for
logging, and for the round-trip tests. **This is the answer to "how do you know
the transaction codes"**: you don't write them, you read them out of the same DEX
you are executing.

### 3.6 Reply construction: two mechanisms, one invariant

Invariant: *the only code that writes a parcelable's bytes is that parcelable's
own `writeToParcel`* unless a generated codec has been differential-tested
against it.

1. **Materialise-and-write (default for complex types).** The Rust service builds
   a *typed description* (`PmsAppInfo { package: "...", source_dir: "...", uid:
   10192, ... }`), the transport materialises the Java object through the VM
   (`new_instance(ApplicationInfo)`, set fields), then calls
   `ApplicationInfo.writeToParcel(reply, 0)` — i.e. the framework marshals its own
   object. Cost: one object per call (3 `ApplicationInfo` calls at launch). Risk
   removed: field order, conditional fields, `Bundle`/`List` nesting.
2. **Generated Rust codec for scalars and small value types** (`Rect`, `Point`,
   `Size`, `RemoteCallback`-free `Bundle` with scalars). Cheaper, and trivial to
   differential-test because the Java reader immediately validates it
   (`enforceNoDataAvail` in the stub throws if we wrote too much).

For *reads* (incoming `addToDisplayAsUser`/`relayout` arguments) the Rust side may
decode **partially**: only the fields the mini-WMS needs, then
`setDataPosition` past the rest. This is legitimate because the stub's
`enforceNoDataAvail()` (which the *caller side* would run) is on the Java side
that we replaced; but it must be listed as an intentional relaxation (risk R-02).

### 3.7 A measured correction to the work-list: `IContentProvider`

The parser (`tools/oracle/binder_from_trace.py`) attributes an outgoing
transaction only to classes ending in `$Stub$Proxy`. Reconciling counts:

* launch: 73 `BinderProxy.transactNative` entries, 68 attributed to the 51
  `$Stub$Proxy` rows → **5 missing**;
* interaction: 106 entries, 102 attributed → **4 missing**.

The missing 5/4 are exactly `android.content.ContentProviderProxy.call`
(MEASURED: 5 launch / 4 interaction), the **hand-written** provider proxy used by
`Settings$NameValueCache`. So the launch interface count is **19 AIDL interfaces
plus `IContentProvider`**, and the Settings provider is a real service ACOS must
implement (see `services.tsv`, `svc.provider`). Without it the app cannot read
`Settings.Global` (animation scale, a11y, device config) and the observe/notify
path in interaction breaks. This also explains `MemoryIntArray.nativeOpen/
nativeSize/nativeGet` and `Parcel.nativeReadFileDescriptor`: the provider hands
back a **file-descriptor-backed generation counter**, which forces real fd
support in the Parcel layer (§3.3).

---

## 4. Service catalogue (per interface, launch **and** interaction)

Machine-readable: **`services.tsv`** (108 rows: 83 outgoing method rows covering
**every** measured transaction, 25 incoming rows over 11 interfaces). Columns: interface,
direction, owner, method, signature, launch_calls, interaction_calls,
state (STATELESS/STATEFUL), reply_or_delivery, consumer, risk, notes.

Owners (Rust components, §6.4): `svc.registry`, `svc.display`, `svc.pms`,
`svc.atms`, `svc.wms`, `svc.input`, `svc.ime`, `svc.vsync`, plus small
`svc.misc`/`svc.a11y`/`svc.textservices`/`svc.autofill`/`svc.provider`/
`svc.contentservice`/`svc.audio`/`svc.futures`/`svc.gfxstats`/`svc.net`.

### 4.1 Interfaces and methods (launch, in first-call order)

Counts are MEASURED `count` values; `risk` is H/M/L from `services.tsv`;
statefulness is the design decision.

| # | interface (owner) | method | n | state | the reply ACOS must produce (hello-views, 1080×2400@420dpi) | why the framework cares | risk |
|--:|---|---|--:|---|---|---|---|
| 1 | `IDisplayManager` (`display`) | `getPreferredWideGamutColorSpaceId` | 1 | stateless | `1` (ColorSpace SRGB) — INFERRED | wide-gamut query only | L |
| 2 | `IDisplayManager` | `getOverlaySupport` | 1 | stateless | `OverlayProperties{fp16=false, mixedColorSpaces=false}`; read back by *our* native, so ACOS owns this format | HDR capability probing | L |
| 3 | `IDisplayManager` | `getDisplayInfo` | 1 | stateless | `DisplayInfo{id=0, name="Built-in Screen", type=1, state=2, logical 1080×2400, app 1080×2209, rotation=0, densityDpi=420, refreshRate=60.000004, modeId=1, colorMode=sRGB}` — DERIVED from `display.txt` | drives `Display`, `Resources` metrics, `Configuration` | M |
| 4 | `INetworkManagementService` (`net`) | `setUidCleartextNetworkPolicy` | 1 | stateless | accept (oneway) | cleartext policy | L |
| 5 | `IPackageManager` (`pms`) | `notifyDexLoad` | 1 | stateless | accept (oneway) | dexopt bookkeeping | L |
| 6 | `IActivityManager` (`atms`) | `checkPermission` | 4 | stateless | manifest+grant driven; `PERMISSION_GRANTED(0)`/`DENIED(-1)` | Settings/DeviceConfig read enforcement | M |
| 7 | `IConnectivityManager` (`net`) | `getProxyForNetwork` | 1 | stateless | `null` | proxy defaults | L |
| 8 | `IPackageManager` | `getPackagesForUid` | 1 | stateless | `["dev.acos.fixtures.helloviews"]` | package attribution | L |
| 9 | `IPackageManager` | `getApplicationInfo` | 2 | stateless | `ApplicationInfo` from the APK manifest: `sourceDir`, `dataDir=/data/user/0/<pkg>`, `nativeLibraryDir`, `uid=10192` (MEASURED in `display.txt` as `A=10192`), `targetSdk=34`, `minSdk=26`, `className`, `processName`, `theme`, `enabled=true` | builds `LoadedApk`: class loader, data dir, native dir — the gate to everything | **H** |
| 10 | `IServiceManager` (`registry`) | `checkService` | 9 | **stateful** | binder by name, `null` if unknown | the chokepoint for all 19 interfaces | M |
| 11 | `IGameManagerService` (`misc`) | `isAngleEnabled` | 1 | stateless | `false` | ANGLE decision | L |
| 12 | `IPackageManager` | `hasSystemFeature` | 3 | stateless | feature table (`touchscreen=true`, `webview=false`, …) | gates widget/IME/animation behaviour | M |
| 13 | `IPackageManager` | `getPackageInfo` | 1 | stateless | `PackageInfo{versionCode=1, versionName, activities=[MainActivity], applicationInfo, requestedPermissions, signatures=[synthetic]}` | version/activity metadata | **H** |
| 14 | `IActivityManager` | `finishAttachApplication` | 1 | **stateful** | ack + record `startSeq`; triggers `bindApplication` delivery | attach handshake | M |
| 15 | `IWindowManager` (`wms`) | `getCurrentAnimatorScale` | 1 | stateless | `1.0f` (INFERRED: no animation-scale override on the reference device; `device-config.json` only records the font scale as default) | animation durations | L |
| 16 | `IWindowManager` | `useBLAST` | 1 | stateless | `true` | selects the BLAST surface path | L |
| 17 | `IDisplayManager` | `registerCallbackWithEventMask` | 1 | **stateful** | record callback; emit nothing | display change listener | L |
| 18 | `IActivityTaskManager` (`atms`) | `getActivityClientController` | 1 | stateless | binder to our controller | lifecycle bookkeeping | L |
| 19 | `IActivityClientController` (`atms`) | `getDisplayId` | 1 | **stateful** | `0` | window/activity display attribution | L |
| 20 | `IVirtualDeviceManager` (`misc`) | `getDeviceIdForDisplayId` | 2 | stateless | `0` | device-id attribution | L |
| 21 | `IActivityManager` | `getContentProvider` | 1 | **stateful** | `ContentProviderHolder{provider=<our IContentProvider>, info=ProviderInfo{authority="settings",…}, delay=false}` | enables `Settings` reads | **H** |
| 22 | `IWindowManager` | `hasNavigationBar` | 1 | stateless | `true` (MEASURED: `NavigationBar0`, 63 px inset) | layout/insets | L |
| 23 | `IAccessibilityManager` (`a11y`) | `addClient` | 1 | **stateful** | clientId=1 + `setState(disabled)` | a11y off (as on the reference) | M |
| 24 | `IAccessibilityManager` | `getRecommendedTimeoutMillis` | 1 | stateless | `0` | timeout extension | L |
| 25 | `IAccessibilityManager` | `getFocusStrokeWidth` | 1 | stateless | `0` | focus ring | L |
| 26 | `IAccessibilityManager` | `getFocusColor` | 1 | stateless | `0` | focus ring | L |
| 27 | `IActivityClientController` | `setTaskDescription` | 1 | stateless | accept; `TaskDescription` materialised | recents | L |
| 28 | `IInputMethodManager` (`ime`) | `addClient` | 1 | **stateful** | record client + remote connection | IME plumbing | M |
| 29 | `IActivityClientController` | `reportSizeConfigurations` | 1 | stateless | accept | size-config buckets | L |
| 30 | `IVoiceInteractionManagerService` (`misc`) | `notifyActivityEventChanged` | 2 | stateless | no-op | voice session events | L |
| 31 | `IWindowManager` | `openSession` | 1 | **stateful** | session id 0 + callback recorded | `IWindowSession` for every window op | M |
| 32 | `IDisplayManager` | `getDisplayIds` | 1 | stateless | `[0]` | display enumeration | L |
| 33 | `IActivityManager` | `setRenderThread` | 1 | **stateful** | record render-thread tid | render-thread attribution | L |
| 34 | `IGraphicsStats` (`gfxstats`) | `requestBufferForProcess` | 1 | **stateful** | memfd `ParcelFileDescriptor` (ProcessStatsBuffer size) → `HardwareRenderer.nSetProcessStatsBuffer` (MEASURED) | frame stats | M |
| 35 | `IWindowSession` (`wms`) | `addToDisplayAsUser` | 1 | **stateful** | window record; **out**: new `InputChannel` (socketpair + token), `InsetsState` (status 0,0–1080,128; nav 0,2337–1080,2400; ime 0,2274–1080,2400 invisible; cutout 0,0–1080,128), `InsetsSourceControl[]`, `outFrame=[0,128][1080,2337]`, `float[]`; return `ADD_OKAY(0)` | attaches the window and gives it input + insets | **H** |
| 36 | `IInputMethodManager` | `isImeTraceEnabled` | 1 | stateless | `false` | tracing | L |
| 37 | `IActivityClientController` | `activityResumed` | 1 | **stateful** | resumed=true | "interactive" signal | M |
| 38 | `IContentCaptureManager` (`misc`) | `registerContentCaptureOptionsCallback` | 1 | stateless | record; never emit | content capture | L |
| 39 | `ITextServicesManager` (`textservices`) | `getCurrentSpellCheckerSubtype` | 2 | stateless | `null` | spell check | L |
| 40 | `IWindowSession` | `setOnBackInvokedCallbackInfo` | 1 | **stateful** | record | predictive back | M |
| 41 | `IWindowSession` | `relayout` | 1 | **stateful** | honor `width/height/flags`; **out**: `ClientWindowFrames{frame=[0,128][1080,2337], displayFrame=[0,0][1080,2400]}`, `MergedConfiguration`, **new `SurfaceControl`**, `InsetsState`, `InsetsSourceControl[]`, `Bundle`; return `0`/`RELAYOUT_RES_FIRST_TIME` | creates/updates the app surface; next step is drawing | **H** |
| 42 | `IWindowManager` | `isInTouchMode` | 1 | stateless | `true` | focus behaviour | L |
| 43 | `IWindowSession` | `finishDrawing` | 1 | **stateful** | apply the client `SurfaceControl.Transaction`; frame_seq++, content hash, wake frame waiters, schedule frame-complete | **the frame is presented here** | **H** |
| 44 | `IInputManager` (`input`) | `getVelocityTrackerStrategy` | 1 | stateless | `"impulse"` | velocity tracking | L |
| 45 | `IInputManager` | `registerInputDevicesChangedListener` | 1 | stateless | record; emit nothing | device list | L |
| 46 | `IInputManager` | `getInputDeviceIds` | 1 | stateless | `[0]` | device list | L |
| 47 | `IInputManager` | `getInputDevice` | 1 | stateless | `InputDevice{id=0, name="ACOS Touch", sources=TOUCHSCREEN|KEYBOARD, motion ranges X:−1..1080, Y:−1..2400}` | coordinate normalisation + `InputDevice` used by `ViewConfiguration` | M |
| 48 | `IInputMethodManager` | `startInputOrWindowGainedFocus` | 1 | **stateful** | V1: `InputBindResult{resultCode=NO_IME}` and no session (declared divergence); faithful variant returns a session and `onBindMethod` | editing state, `TextView` focus | **H** |
| 49 | `IInputMethodManager` | `reportPerceptibleAsync` | 1 | stateless | no-op | insets perceptibility | L |
| 50 | `IInputMethodManager` | `removeImeSurfaceFromWindowAsync` | 1 | stateless | no-op | IME surface | L |
| 51 | `IActivityClientController` | `activityIdle` | 1 | **stateful** | idle=true; completes "launched" | launch completion | M |
| + | `IContentProvider` (`provider`) | `call` (**5, unlisted**) | 5 | **stateful** | Settings semantics: `GET_global/GET_secure/GET_system` → `Bundle{value}` + a `ParcelFileDescriptor` for the generation `MemoryIntArray`; `PUT_*` bumps the generation | `Settings$NameValueCache`, `GenerationTracker` | **H** |

### 4.2 Interaction-only additions (13 interfaces total, 30 methods)

| interface | methods (n) | state | reply | risk |
|---|---|---|---|---|
| `IAutoFillManager` | `addClient`(1), `startSession`(1) | stateful | autofill disabled → finish the session via the `IResultReceiver` immediately | M |
| `IPackageManager` | `hasSystemFeature`(1) | stateless | as above | M |
| `IInputMethodManager` | `startInputOrWindowGainedFocus`(1), `getImeTrackerService`(1), `showSoftInput`(1), `hideSoftInput`(1), `reportPerceptibleAsync`(6), `removeImeSurfaceFromWindowAsync`(1) | stateful | no IME; **always answer the `ResultReceiver`** | M/H |
| `IInputMethodSession` | `viewClicked`(1), `updateSelection`(11) | stateless | no-op oneway | L |
| `IImeTracker` | `onRequestShow`(1), `onProgress`(22), `onShown`(1), `onRequestHide`(1), `onHidden`(1) | stateful | monotonic `Token` ids; record | L |
| `IActivityTaskManager` | `reportAssistContextExtras`(1) | stateless | drop | L |
| `IWindowSession` | `setOnBackInvokedCallbackInfo`(2), `updateRequestedVisibleTypes`(2), `onRectangleOnScreenRequested`(**18**) | stateful | no-op except recording | L |
| `IContentService` | `registerContentObserver`(1) | stateful | record; emit `onChange` only on real change | M |
| `IWindowManager` | `setInTouchMode`(2) | stateless | accept | L |
| `ITextServicesManager` | `isSpellCheckerEnabled`(5), `getCurrentSpellCheckerSubtype`(6), `getCurrentSpellChecker`(1), `getSpellCheckerService`(1) | stateless | null / false; **never leave the session listener hanging** | M |
| `IAndroidFuture` | `complete`(8) | stateless | drop: ACOS never blocks on a future | L |
| `IServiceManager` | `checkService`(1) | stateful | as above | M |
| `IAudioService` | `areNavigationRepeatSoundEffectsEnabled`(1), `playSoundEffect`(2) | stateless | false / drop (declared: silent) | L |
| `IContentProvider` | `call`(4, unlisted) | stateful | as above | H |

### 4.3 Incoming callbacks and their sequencing

MEASURED incoming rows: launch 3 transports (`IWindow` ×2 calls,
`IApplicationThread` ×1, `IInputMethodClient` ×3) + 5 handler rows = 8 rows;
interaction **6 incoming interfaces** (`IWindow` 13, `IRemoteInputConnection` 15,
`IResultReceiver` 4, `IApplicationThread` 2, `IAutoFillManagerClient` 1,
`ITextServicesSessionListener` 1) + 14 handler rows = 20 rows. (The earlier
`research/dynamic-trace/REPORT.md` calls this "11 incoming transports"; that
counts transports and handler methods with a different convention — the row
counts above are the raw TSV rows and are what `services.tsv` enumerates.)

Sequence that ACOS must synthesise (framework entries named; Rust in brackets):

```
[atms] attachApplication received  (app -> system, pre-profile)
  [atms] build AppBindData{ApplicationInfo, providers, Configuration, CompatibilityInfo, appClass}
  [atms] -> IApplicationThread.bindApplication                       (system -> app, MUST be next)
      ActivityThread.handleBindApplication -> LoadedApk, Application.attach, Application.onCreate
  [app]  finishAttachApplication                                      (app -> system)
  [atms] ClientTransaction{LaunchActivityItem, ResumeActivityItem} -> scheduleTransaction
      Activity.performCreate/onCreate -> onStart -> onResume
  [app]  IWindowManager.openSession -> addToDisplayAsUser             (app -> wms)
  [wms]  -> IWindow.dispatchAppVisibility / windowFocusChanged / insetsControlChanged  (system -> app)
  [app]  IWindowSession.relayout                                      (app -> wms)
  [wms]  -> IWindow.resized (ClientWindowFrames + Configuration)
  [app]  Choreographer frame -> ThreadedRenderer.draw -> finishDrawing (app -> wms)
  [wms]  compositor applies transaction, frame_seq++, schedules frame complete
  [wms]  -> IApplicationThread.scheduleEnterAnimationComplete         (MEASURED at launch)
  [app]  activityIdle                                                 (app -> system)
  [atms] launch complete -> the scenario runner may observe/act
```

Interaction adds (in MEASURED order): `IAutoFillManager.addClient/startSession` →
`IResultReceiver.send`; `IApplicationThread.requestAssistContextExtras`; IME
`startInputOrWindowGainedFocus` → `IInputMethodClient.setActive/onBindMethod/
reportFullscreenMode`; `IWindowSession.setOnBackInvokedCallbackInfo`; the input
pipeline `InputChannel` → `InputEventReceiver` → `MotionEvent` → view dispatch →
`IWindowSession.onRectangleOnScreenRequested` (18×) → IME
`updateSelection`/`reportPerceptibleAsync` → `IImeTracker.onProgress` → back/
`hideSoftInput` → `IWindow.hideInsets`.

Rules: (1) all incoming transports are delivered **through the target looper's
message queue**; (2) a `oneway` transport must not be delivered before the
`relayout` reply it logically follows (queue order does this naturally);
(3) `IResultReceiver.send` and `IAndroidFuture.complete` must always be answered
or the app will wait — the two hang risks of the whole design.

---

## 5. Ambient state: properties, Build, Settings, resources, configuration

### 5.1 What is measured

| surface | MEASURED evidence | what ACOS must provide |
|---|---|---|
| `SystemProperties` | 6 natives (`native_get` 9, `native_get_long` 49, `native_get_boolean` 5, `native_find` 5, `native_get_int` 2, `native_get_long` 1) + 14 framework methods in `channels_*` | a property table seeded from `.cache/boot/android-34/build.prop` (5,008 B, `ro.build.version.sdk=34`, fingerprint `google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys`) plus ACOS overrides; `native_find` returns a stable `SystemProperties$Handle` |
| `Build` | not directly traced (static initialisers are in the boot image) | all `Build.*` fields from `build.prop`; `Build.FINGERPRINT`, `Build.VERSION.SDK_INT=34`, `Build.TYPE="userdebug"`, `Build.HARDWARE`, `Build.SUPPORTED_ABIS=["x86_64"]` |
| `Settings.Secure/Global/System` | `Settings$NameValueCache` 14 `getStrings`, `Settings$Global.getString/int` 3/2, `Settings$GenerationTracker` 15+15, `IContentProvider.call` ×5, `MemoryIntArray` ×15+15+5 | the `svc.provider` Settings table (§3.7) with the generation counter |
| `DeviceConfig` | `<clinit>` after fork + `Settings` calls | a defaults-only table (all namespaces empty) |
| resources | `AssetManager` 21 natives (1,342 enters), `XmlBlock` 11 (226 `nativeNext`), `StringBlock` 3, `TypedArray.obtain` 213 | framework-res.apk (**34.4 MB, 7,880 files**) loaded via `ApkAssets.nativeLoad` into an ACOS asset source; `android:` theme/style attrs come from here |
| fonts/typefaces | `Typeface` 8 natives, `nativeAddFontCollections` 33, `nativeReadTypefaces` 1 | `.cache/boot/android-34/fonts/` (**75 MB**, `fonts.xml`); `Typeface.init` → `acos-render` `FontCollection` |
| locale | `com.android.icu.util.LocaleNative.setDefaultNative` 3, `libcore.icu.ICU.setDefaultLocale` | `LocaleList` = `[en-US]` (MEASURED `device-config.json`; `display.txt` config `[en_US]`) |
| configuration | `AssetManager.nativeSetConfiguration` 4, `nativeGetSizeConfigurations` 1 | `{mcc310 mnc260 [en_US] ldltr sw411dp w411dp h842dp 420dpi nrml long port finger qwerty/v/v dpad/v}` — **MEASURED verbatim in `display.txt`**, app bounds `[0,128][1080,2337]` |
| display metrics | `DisplayManagerGlobal.getDisplayInfo` 24, `DisplayInfo.getMetricsWithSize` 5 | `densityDpi=420, density=2.625, scaledDensity=2.625, xdpi/ydpi≈420 (INFERRED), widthPixels=1080, heightPixels=2400` |
| ICU | `NativeConverter` 9 natives (openConverter/encode/decode callbacks 16/8), `Character.getTypeImpl` etc. 7 | converter registry (UTF-8/UTF-16/Latin-1/ASCII) in Rust; `Character` tables |
| ICU4J **data** | interaction: `android.icu.impl.ICUBinary$PackageDataFile.getData`, `RBNF`/`RuleBasedBreakIterator` (149 `android.icu` methods, 657 enters), `FileInputStream.available0` 2 | **GAP G-1**: the ICU data package file is not in `.cache/boot/android-34`; ACOS must locate and provide it |

### 5.2 Gaps and the exact measurement to close them

**G-1 (ICU data path).** ICU4J lazily loads its rule data at the first text break
(interaction only), via `ICUBinary.getData` reading a `.dat` package from
`/usr/icu/` or `ICU_DATA`. Not in the extracted cache. Measure:

```bash
# 1. does the file exist, and where?
adb -s emulator-5554 shell 'ls -l /apex/com.android.i18n/etc/icu/ 2>/dev/null; ls -l /system/icu/ 2>/dev/null; getprop | grep -i icu'
# 2. confirm the reader and the path it opens (root shell needed; userdebug image)
adb -s emulator-5554 root
adb -s emulator-5554 shell 'am force-stop dev.acos.fixtures.helloviews'
adb -s emulator-5554 shell 'setprop wrap.dev.acos.fixtures.helloviews "logwrapper strace -f -e trace=openat,mmap -o /data/local/tmp/icu.strace"'
adb -s emulator-5554 shell 'am start -W -n dev.acos.fixtures.helloviews/.MainActivity'
adb -s emulator-5554 shell 'input tap 540 427; input text World'   # forces the first line break
adb -s emulator-5554 shell 'grep -E "icu.*\.dat|\\\.dat" /data/local/tmp/icu.strace'
adb -s emulator-5554 shell 'setprop wrap.dev.acos.fixtures.helloviews ""'
```

**G-2 (which system properties are read).** Native arguments are not in the
method trace, and bionic reads the property area from a shared mapping (so
`strace` would not show it). Two cheap measurements, neither of which needs a
rebuild:

```bash
# (a) static: the property *names* are const-string literals at the call sites of
#     SystemProperties.get*/find in the executed method set. Decode those with
#     acos-dex and join against data/methods_hello_views_launch.tsv:
#     tools/oracle/prop-keys.py (to be written; backlog T-31)
# (b) dynamic (precise, needs NDK, no download):
#     build a tiny libprop_shim.so exporting __system_property_read_callback /
#     __system_property_find, LD_PRELOAD it via `wrap.<pkg>`, log key names.
adb -s emulator-5554 shell 'setprop wrap.dev.acos.fixtures.helloviews "logwrapper LD_PRELOAD=/data/local/tmp/libpropshim.so"'
adb -s emulator-5554 shell 'logcat -d -s prop-shim'   # key + value per read
```
Until then ACOS serves a **complete** property namespace (all of `build.prop` plus
the standard `debug.hwui.*`, `ro.*`, `persist.sys.*` defaults), so an unknown key
returns `""`/the caller's default exactly as a real device would for an unset
property — a safe default that does not fake values.

**G-3 (`hasSystemFeature` names).** Same class of gap (argument values untraced).
Cheap closure without a rebuild: ACOS logs every `hasSystemFeature` name at INFO
and the *first* run's log becomes the table. Meanwhile the table is
conservative-false except for touch.

**G-4 (`DisplayInfo`/`PackageInfo`/`ApplicationInfo` field sweeps).** Not a data
gap but a codegen gap: the exact write order is in the DEX. See §3.6 and backlog
T-08 (codec generator + differential tests).

**G-5 (thread inventory).** MEASURED: the interaction trace runs on 2 Java
threads (main 13403, render 13417); launch shows main only. Unknown: whether any
`HandlerThread` (e.g. `android.bg`, `hwuiTask0`) is created and *waited on* by
the launch path. Measure: `adb shell 'ps -T -p <pid>'` (and `/proc/<pid>/task/*/
comm`) at the moment `am start -W` returns, repeated 5×. Until then ACOS
implements the two threads it knows about plus `HandlerThread` as a generic VM
thread.

---

## 6. Zygote replacement

### 6.1 What zygote actually gives ART (and therefore what ACOS must do)

MEASURED (REPORT Part A/B): 1,076 of 1,128 executed classes are in
`/system/etc/preloaded-classes` (**17,367 lines / 17,344 class descriptors**,
extracted to `.cache/boot/android-34/preloaded-classes`); only 12 classes run `<clinit>` after
fork on the reference device; the boot image is pre-*initialised*, so the true
from-scratch `<clinit>` sequence is **not observable** (GAP G-6, below). ACOS has
no boot image, so for every class it touches it must run what the image hid:

1. **Class loading + linking** for the preload set (class metadata, resolved
   strings/methods/fields).
2. **`<clinit>`** for every class in that set that declares one, in an order that
   satisfies: superclass before subclass; a class's `<clinit>` before its first
   *active* use (new-instance, static field access, static method invoke,
   subclass init); interface `<clinit>` before the initialisation of an
   implementing class that uses its constants.
3. **Framework statics that zygote sets up**: system `Resources` +
   `AssetManager` for `framework-res.apk`; `Typeface` defaults (`nativeSetDefault`,
   `nativeAddFontCollections`, `nativeRegisterGenericFamily`, `nativeReadTypefaces`);
   `LocaleList.setDefault` + `libcore.icu.ICU.setDefaultLocale`;
   `TimeZone`/tzdata (`LocaleData`); `SystemProperties` first read;
   `VMRuntime.setTargetSdkVersion/setProcessPackageName/registerAppInfo`;
   `VMRuntime.clampGrowthLimit`; `BlockGuard`/`StrictMode` defaults;
   `Build` static init; `Charset` provider registration; `sun.misc.Unsafe` /
   `jdk.internal.misc.Unsafe` offset bootstrap; `VMClassLoader` boot class path;
   `Process.setArgV0`; and signal handlers (SIGSEGV/GC-dump — ACOS installs its
   own trap handlers in Rust, not bionic's).
4. **The app process start** (`ZygoteInit → RuntimeInit → ActivityThread.main`):
   ACOS uses **`ActivityThread.main(new String[0])`** — *not* `systemMain`
   (which is the system_server path and would try to become system_server).
   `main` calls `Looper.prepareMainLooper()` and `attach(false)`, which is where
   the mini-ATMS takes over (§4.3).

### 6.2 Ordering constraints (actionable)

* Superclass chain first, then the class; `<clinit>` runs under the VM's class
  init lock with recursive-init prevention (a `<clinit>` that re-enters its own
  class must not deadlock).
* The measured trace's `first_order` column is an **order witness**: the first
  time a method of class *C* appears is a lower bound on when *C* was
  initialised. Use it to (a) generate the snapshot build order, (b) *assert* the
  rebuilt snapshot's init order is consistent with the witness (a class must not
  be initialised later than its first measured use, and must not be initialised
  earlier than the first use of a class it depends on).
* Classes with no `<clinit>` still need *loading* before use, but not
  initialisation; do not force-initialise the whole preload list (that is the
  measured 17,367-class zygote cost, ~hundreds of ms; ACOS should initialise the
  **reachable** closure only — 1,128 classes + their init closure for
  hello-views).

### 6.3 Snapshot design (ADR-0010)

**Contents (serialised, generation-tagged):**

1. Class metadata for the snapshot class set: descriptors, super/interface
   links, vtables/itable stubs, field layout, method table, native bindings,
   dex origin (path + entry hash), verification state.
2. Initialised static state: every static field of every initialised class
   (object graph included), i.e. the *result* of `<clinit>`.
3. Heap arena image + handle table + mark bits, with the GC roots explicitly
   listed (static fields, interned strings, native roots, `Typeface`/
   `AssetManager`/`Resources`/`FontCollection`/ICU converter objects).
4. Interned string table (identity matters: `intern()` returns the same object).
5. Object identity hash codes (persisted, **not** regenerated — `identityHashCode`
   is observable and a map iteration order can depend on it).
6. Snapshot metadata: build fingerprint, property table hash, framework-res hash,
   locale, config, monotonic clock anchor (`uptimeMillis` origin), snapshot
   format version.

**Must NOT be in the snapshot:**

* any `Looper`, `MessageQueue`, `Handler`, `Choreographer` or `InputEventReceiver`
  (per-process/per-thread; a restored Looper would have fds pointing nowhere);
* `ActivityThread`, `Activity`, `Context`, `Window`, `View`, `ViewRootImpl`,
  `Application`, `LoadedApk` — per-app state, created after restore;
* pids/tids, `/proc/self` paths, thread names, `Process.myPid()` caches;
* file descriptors and anything holding them (`AssetManager` keeps the APK fd
  open! the snapshot must store the *path* and reopen, or accept that
  `AssetManager` is created per process);
* randomness state (`java.util.concurrent.ThreadLocalRandom` — MEASURED to
  `<clinit>` during interaction) and any static holding a random seed;
* wall-clock values (`currentTimeMillis` cached in statics by e.g.
  `DeviceConfig`, `Settings$GenerationTracker`, `Trace`), except as an offset;
* monitors/locks in a held state, parked threads, JNI local reference tables,
  pending exceptions;
* JIT code / profiling state / `VMDebug` tracing state.

**Re-seed hooks after restore (must be a single, tested list):**

| hook | why |
|---|---|
| pid/tid/`Process.myPid/myTid/myUid` | identity is per process; uid stays 10192 |
| `ThreadLocalRandom` / `Random` seeds | determinism vs. the reference |
| `SystemClock` anchor + `System.nanoTime` offset | monotonic time must not jump backwards |
| `SystemProperties` cache + `Build` (they cache at init) | ACOS may override properties per scenario |
| `LocaleList`/ICU default locale, `TimeZone` | per-task configuration |
| `Resources`/`Configuration`/`DisplayMetrics` | per-task display + orientation |
| `Binder` per-thread caller identity stack | empty after restore |
| file-descriptor table, `AssetManager`/`ApkAssets` handles | fds do not survive |
| `ActivityThread.currentActivityThread()` | must be null until the app attaches |
| GC "current thread"/allocation counters, `VMRuntime` limits | per process |

**Invariants to check (each becomes a test):**

1. **No threads**: restoring must not resurrect a `Thread` object marked alive;
   `Thread.getAllStackTraces()` after restore returns only ACOS-created threads.
2. **No fds**: the snapshot contains no fd number; restore opens zero fds except
   the ones it explicitly re-establishes.
3. **Determinism**: two restores of the same snapshot, running the same pure-Java
   scenario, produce byte-identical output and identical
   `System.identityHashCode` for the same objects.
4. **Init completeness**: for every class in the snapshot set,
   `Class.isInitialized()` is true (or `<clinit>` is run at first use, recorded),
   and no `<clinit>` runs for a snapshot class after restore.
5. **Idempotence**: taking a snapshot of a restored process and restoring *that*
   yields an equivalent state (fixpoint test).
6. **Memory**: copy-on-write sharing is measurable — N workers from one snapshot
   must show per-worker RSS well below N × full RSS, and PSS must grow
   sub-linearly.
7. **Roots**: `snapshot.roots == GC roots after restore`; no unreachable objects
   in the snapshot (the "no garbage in the image" test).
8. **No per-app leakage**: two workers run different scenarios concurrently with
   independent activity state (`docs/V1.md` 5.1).

**GAP G-6 (the from-scratch `<clinit>` count).** Not observable on the reference
device (the boot image is pre-initialised; `-Ximage`/`-Xnoimage-dex2oat` do not
disable it). ACOS's own first snapshot build measures it directly (log every
`<clinit>`), and that number becomes the baseline to optimise. Estimated from
DEX (DERIVED): classes with `<clinit>` in the executed set — compute with
`tools/oracle/dexclosure.py --clinit-join` (backlog T-32); the static inventory
says 5,932 boot-classpath classes have `<clinit>`, so the reachable subset is
expected in the low hundreds, not thousands.

---

## 7. Startup sequence for an app in ACOS (pseudo-code)

`[R]` = Rust service layer, `[F]` = real framework bytecode, `[N]` = native
(Rust implementation of a framework native).

```
 1 [R] acos-runtime::boot(task):
       load snapshot (§6.3)                      # classes+statics+heap, no threads, no fds
       re-seed hooks                              # pid/tid/time/locale/props/fds
       create process record: pid, uid=10192, package=dev.acos.fixtures.helloviews
       open APK, parse manifest + resources (acos-apk)
       start Rust components: registry, display, pms, atms, wms, input, vsync, provider
 2 [R] -> start VM thread "main":
       [F] ActivityThread.main(new String[0])
           Looper.prepareMainLooper()                        [F]  -> [N] MessageQueue.nativeInit
           ActivityThread.attach(false)                      [F]
             -> IActivityManager.attachApplication(mAppThread, seq)   [F proxy] -> [R atms]
 3 [R] atms: record app thread binder; build AppBindData (ApplicationInfo from pms);
       materialise the Java AppBindData; enqueue BinderDelivery on the main looper:
       [F] IApplicationThread.bindApplication(...)   (delivered via execTransact -> onTransact)
           handleBindApplication                             [F]
             -> LoadedApk construction (sourceDir, dataDir, nativeLibraryDir)   [F]
             -> ClassLoader for the APK (ours)              [F] -> [N] DexFile.openDexFileNative
             -> Application.attach(context) -> Application.onCreate             [F]
 4 [F] -> IActivityManager.finishAttachApplication(seq)      [F proxy] -> [R atms]
 5 [R] atms: ClientTransaction{LaunchActivityItem, ResumeActivityItem} ->
           IApplicationThread.scheduleTransaction  (delivered on the main looper)
       [F] TransactionExecutor -> Activity.performCreate/onCreate/onStart/onResume
           setContentView -> LayoutInflater.inflate -> Resources/Theme/TypedArray
             -> [N] AssetManager.nativeGetResourceValue/applyStyle/retrieveAttributes
             -> [N] XmlBlock.nativeCreateParseState/nativeNext/...   (AXML pull parser)
             -> TextView/Button/EditText construction, Paint/Typeface init
                 -> [N] Typeface.nativeAddFontCollections / Paint.nInit
 6 [F] Activity.onResume -> ViewRootImpl.setView
       -> IWindowManager.openSession                [F proxy] -> [R wms] (session 0)
       -> IWindowSession.addToDisplayAsUser         [F proxy] -> [R wms]
            [R] create window record; create InputChannel (socketpair) + token binder;
                write InsetsState (status 0,0-1080,128 / nav 0,2337-1080,2400 / ime hidden),
                InsetsSourceControl[], outFrame [0,128][1080,2337]
       -> [N] InputChannel.nativeReadFromParcel  -> [N] InputEventReceiver.nativeInit(fd)
       -> IWindowSession.relayout                   [F proxy] -> [R wms]
            [R] window state machine: size/flags -> frame; NEW SurfaceControl (layer id)
                + ClientWindowFrames + MergedConfiguration + InsetsState
       -> [N] Surface.nativeIsValid -> scheduleTraversals
 7 [F] Choreographer.postCallback(TRAVERSAL)
       -> [N] DisplayEventReceiver.nativeScheduleVsync
            [R vsync] arm the next 60 Hz deadline (16.667 ms grid, boot-anchored);
                      when idle: no timer at all, epoll waits forever
       -> [N] MessageQueue.nativePollOnce(fd, -1)   <- the only blocking point
            [R] epoll: {mq wake fd, input fd, vsync timerfd, binder queue condvar}
 8 vsync fires -> [F] Choreographer.doFrame -> ViewRootImpl.performTraversals
       performMeasure  -> View.measure -> [N] text metrics (LineBreaker/Paint) 
       performLayout   -> View.layout  -> [N] RenderNode.nSetLeftTopRightBottom
       performDraw     -> ThreadedRenderer.draw -> HardwareRenderer.nSyncAndDrawFrame
            [R] render thread: replay DisplayOp list into acos-render, rasterise
 9 [F] ViewRootImpl.finishDrawing
       -> IWindowSession.finishDrawing(IWindow, SurfaceControl.Transaction, seq)
            [R wms/compositor] apply transaction, take the rendered buffer,
                 frame_seq++, content hash, frame class (DRAWN vs UNCHANGED),
                 signal the frame waiter (agent-facing `capture --fresh`),
                 schedule frame-complete back to the app
10 [F] acknowledge frame -> IApplicationThread.scheduleEnterAnimationComplete  [R atms]
11 [F] -> IActivityClientController.activityIdle(...)    -> [R atms] launch complete
12 [R] task state = READY (tree, frame, input live)
```

What replaces what:

| Android piece | ACOS replacement | framework entry point it feeds |
|---|---|---|
| zygote | boot snapshot (§6) | `ActivityThread.main` |
| AMS/ATMS | `[R] atms` (process record, lifecycle transaction stream) | `attachApplication`, `scheduleTransaction` |
| PMS | `[R] pms` (APK manifest) | `getApplicationInfo`, `getPackageInfo`, `hasSystemFeature` |
| WMS + SurfaceFlinger | `[R] wms` + compositor (window records, layer tree, transactions) | `addToDisplayAsUser`, `relayout`, `finishDrawing` |
| input dispatcher | `[R] input` (channel socketpairs, `InputMessage` framing, injection API) | `InputEventReceiver`, `MotionEvent` |
| Choreographer vsync source | `[R] vsync` (deadline heap + timerfd, armed on demand) | `DisplayEventReceiver.nativeScheduleVsync` |
| DisplayManagerService | `[R] display` | `getDisplayInfo`, `getDisplayIds` |
| IME (InputMethodService) | `[R] ime` (V1: no IME, declared) | `startInputOrWindowGainedFocus` |
| SettingsProvider | `[R] provider` | `IContentProvider.call` |
| HWUI/Skia | `acos-render` | `HardwareRenderer.*`, `Canvas.*`, `RenderNode.*` |

**Vsync/frame design (no timers when idle).** One `epoll` set per process:
`{MessageQueue wake fd, input channel fds, vsync timerfd, binder-delivery
eventfd}`. `nativePollOnce(timeout)` maps to `epoll_wait(timeout)` where the VM
passes `-1` whenever the queue has no due messages — so an idle ACOS process
consumes **no wakeups and no CPU**. The vsync generator has *no periodic timer*:
it computes a deadline only when `nativeScheduleVsync` is called, arms a
`timerfd_settime(ABS)`, and disarms after firing. `Choreographer` stops calling
`scheduleVsync` when nothing is dirty (measured: 6 vsync schedules at launch, 57
over the whole interaction) — so the resting state is genuinely timer-free. No
fixed sleeps anywhere: frame completion is edge-driven (`finishDrawing` →
compositor → completion message), and animation deadlines come from
`RenderNodeAnimator.nSetDuration`/`Choreographer` frame callbacks, which are
already event-driven.

---

## 8. Native-method plan by subsystem

Source of truth: **`natives_plan.tsv`** — 520 rows covering **all** natives in
`natives_activity_launch.tsv` (379), `natives_activity_interaction.tsv` (327) and
`natives_println.tsv` (21), with `module`, `complexity` (S < ~2 h, M ≈ 0.5–1 day,
L > 1 day / design decision embedded), per-phase enter counts, first-order and a
note. Generated by `gen_natives_plan.py`; the module map and per-method
overrides are curated there, so re-running after a new trace is a one-line diff.

### 8.1 Modules, counts, complexity (DERIVED)

| module | natives | L | M | S | launch enters | interaction enters | depends on |
|---|---:|---:|---:|---:|---:|---:|---|
| `core.lang` (String/Object/System/Math/Class/reflect/Throwable) | 47 | 12 | 15 | 20 | 25,365 | 13,056 | acos-vm heap/class model |
| `core.icu` (Character ICU tables) | 5 | 0 | 2 | 3 | 14 | 32 | `text.icu` |
| `core.thread` (Thread/Reference/NativeAllocationRegistry) | 11 | 4 | 6 | 1 | 1,151 | 3,775 | VM threads + GC |
| `core.vmrt` (VMRuntime/Unsafe/DexFile/VMDebug/ClassLoaderFactory/Runtime) | 35 | 6 | 23 | 6 | 344 | 399 | heap pinning, acos-dex |
| `core.io` (Linux/Memory/UnixFileSystem/UNIXProcess/AsynchronousCloseMonitor) | 23 | 2 | 8 | 13 | 162 | 707 | libc, VFS |
| `core.os` (SystemProperties/SystemClock/Trace/Log/Process/EventLog/ActivityThread/Activity) | 18 | 4 | 1 | 13 | 559 | 2,021 | property table, log sink |
| `os.parcel` (**Parcel/Binder/BinderProxy/MemoryIntArray**) | 39 | **20** | 18 | 1 | 3,391 | 6,713 | `svc.registry`, fd table |
| `os.looper` (MessageQueue) | 3 | 2 | 1 | 0 | 36 | 506 | epoll |
| `res` (AssetManager/ApkAssets/StringBlock/XmlBlock) | 38 | 9 | 26 | 3 | 1,342 | 177 | `acos-apk` (arsc/axml) |
| `render.canvas` (Canvas/BaseCanvas/RecordingCanvas/BaseRecordingCanvas) | 22 | 0 | 19 | 3 | 267 | 555 | `acos-render` |
| `render.paint` | 45 | 5 | 2 | 38 | 421 | 2,680 | `acos-render` + text |
| `render.node` (RenderNode, VirtualRefBasePtr) | 32 | 2 | 4 | 26 | 538 | 1,328 | `acos-render` |
| `render.path` / `render.matrix` / `render.region` | 9 / 6 / 2 | 0/0/0 | 7/0/2 | 2/6/0 | 25/11/7 | 72/144/0 | `acos-render` |
| `render.bitmap` (Bitmap/NinePatch/ImageDecoder) | 20 | 2 | 14 | 4 | 67 | 35 | `acos-render::image` |
| `render.shader` (RuntimeShader/BitmapShader) | 6 | 1 | 5 | 0 | 0 | 40 | AGSL decision (below) |
| `render.anim` (RenderNodeAnimator/CanvasProperty/PropertyValuesHolder/Interpolator) | 13 | 3 | 2 | 8 | 14 | 51 | `acos-render` + Choreographer |
| `render.vector` (VectorDrawable/PathParser) | 18 | 2 | 8 | 8 | 288 | 0 | `acos-render` paths |
| `render.hwui` (HardwareRenderer/HardwareRendererObserver) | 32 | 5 | 9 | 18 | 41 | 175 | `acos-render` scene |
| `text.typeface` | 8 | 2 | 6 | 0 | 120 | 0 | `acos-render::fonts` |
| `text.layout` (LineBreaker/MeasuredText) | 12 | 3 | 3 | 6 | 2 | 70 | `acos-render::text::layout` |
| `text.icu` (NativeConverter/LocaleNative/CharsetUtils) | 11 | 3 | 6 | 2 | 15 | 9 | ICU data (G-1) |
| `view.input` (MotionEvent/InputChannel/InputEventReceiver/InputEventSender/Key*) | 26 | 2 | 23 | 1 | 5 | 504 | `svc.input` |
| `view.surface` (SurfaceControl/Surface/BLASTBufferQueue/SurfaceSession/OverlayProperties) | 31 | 8 | 19 | 4 | 64 | 283 | `svc.wms` compositor |
| `view.vsync` (DisplayEventReceiver) | 3 | 3 | 0 | 0 | 8 | 58 | `svc.vsync` |
| `display` (DisplayManagerGlobal) | 1 | 0 | 0 | 1 | 1 | 0 | `svc.display` |
| `render.env` (GraphicsEnvironment) | 4 | 0 | 0 | 4 | 4 | 0 | `acos-render` |
| **total** | **520** | **100** | **229** | **191** | 34,262 | 33,390 | |

### 8.2 Dependency order (implementation waves)

1. **core.lang + core.thread + core.vmrt** (heap, class model, strings, arrays,
   Unsafe offsets, identity hash, Thread/Reference). Nothing else compiles
   conceptually without it; the println milestone needs exactly 21 natives.
2. **os.looper + os.parcel** (epoll + Parcel + Binder). Parcel is
   self-contained and unblocks both Binder and `SurfaceControl` transactions.
3. **core.os + core.io** (properties, clocks, log, posix, mmap). Needed for
   `ActivityThread` and AssetManager (APK fd).
4. **res** (AssetManager/ApkAssets/StringBlock/XmlBlock) — the gate to themes and
   inflation; reuses `crates/acos-apk/{arsc,axml}`.
5. **render.canvas/paint/node/matrix/path/region/bitmap** — first frame.
6. **text.typeface + text.layout (+ text.icu)** — real text metrics; a first frame
   can be drawn with fallback metrics, but `clicks: 0` bounds must match the
   reference so this lands before pixel comparison.
7. **view.surface + view.vsync + display + svc.wms** — window/surface lifecycle.
8. **render.hwui** — the `HardwareRenderer` state machine on top of (7).
9. **view.input** — after the window exists.
10. **render.anim + render.vector + render.shader** — interaction polish.

### 8.3 Graphics mapping to `acos-render` (current API, being written concurrently)

Read from `crates/acos-render/src/` (10,233 lines: `context.rs` 2,202,
`canvas.rs` 898, `text/mod.rs` 905). Mapping rule: **one Rust handle per
framework `jlong`**, owned by `RenderContext`'s generation-checked `HandleTable`,
so a stale `jlong` (which ART would dereference) becomes a structured
`RenderError::StaleHandle` instead of UB.

| framework natives | acos-render target |
|---|---|
| `Canvas.nInitRaster/nSave/nRestore/nRestoreToCount/nTranslate/nScale/nClipRect/nGetClipBounds/nQuickReject` | `Canvas` (`save`/`restore`/`concat`/`clip_rect`/`clip_bounds`/`quick_reject`) |
| `RecordingCanvas.*`, `BaseRecordingCanvas.nDrawRect/nDrawText/nDrawRoundRect/nDrawNinePatch/nDrawTextRun` | `RenderNode` + `DisplayOp` recording (`render_node.rs`), replayed by `Scene` |
| `Paint.nInit/nSet*/nGet*/nSetTextLocalesByMinikinLocaleListId` | `Paint` fields (`paint.rs`), incl. `TextAlign`, caps/joins, hyphen edits (stored, applied at shaping) |
| `Paint.nGetFontMetricsInt(ForText)`, `nGetRunCharacterAdvance`, `nGetCharArrayBounds` | `text::ShapedText`/`FontMetricsInt` (rustybuzz) — **must** agree with Minikin to ±0.01 px for bounds parity |
| `Typeface.nativeCreateFromTypeface/nativeAddFontCollections/nativeReadTypefaces/nativeSetDefault` | `text::fonts::FontCollection` + `FontRef` (mmap of `.cache/boot/android-34/fonts`) |
| `LineBreaker.nInit/nComputeLineBreaks/nGetLine*`, `MeasuredText$Builder.*` | `text::layout::{LineBreaker, LineBreak, ParagraphConstraints}` |
| `RenderNode.*` (30) | `RenderNode` properties + `DisplayOp` list; `nCreate` = allocate handle |
| `HardwareRenderer.nCreateRootRenderNode/nCreateProxy/nSetSurface/nSyncAndDrawFrame/nNotifyFramePending/nAllocateBuffers` | `Scene`/`SceneRender` + `Surface` frame production (render thread) |
| `Bitmap.nativeCreate/nativeRowBytes/nativeHasAlpha/nativeIsPremultiplied/nativeErase/nativeConfig` | `Bitmap`/`BitmapConfig` (RGBA8, CPU) |
| `ImageDecoder.*` | `image.rs` (png/jpeg/webp) |
| `Path.*`, `PathParser.*`, `VectorDrawable.*` | `Path`/`PathMeasure`/`FillType`; `VectorDrawable` = parsed `Path` tree |
| `Matrix.*`, `Region.*` | `Matrix3x3`, `Region` |
| `BlendModeColorFilter`/`PorterDuffColorFilter`/`BlendMode` | `ColorFilterSpec`/`AndroidBlendMode` |
| `RuntimeShader.*` | **decision**: V1 returns a structured `unsupported` and draws the base shape (the ripple is the only user); V2 = AGSL interpreter or a shader→`ShaderSpec` mapper |
| `HardwareRendererObserver.nGetNextBuffer` | frame-timeline samples produced by `svc.vsync` + `Surface::Frame` |
| `GraphicsEnvironment.*` | report acos-render identity (never claim HWUI) |

### 8.4 ICU: measured usage and the recommendation

MEASURED: the **only** ICU natives hit are `com.android.icu.charset.NativeConverter`
(9 methods: `openConverter`, `encode`, `decode`, `setCallbackEncode/Decode`,
`getMaxBytesPerChar`, `getAveBytesPerChar`, `getAveCharsPerByte`,
`getNativeFinalizer`) and `com.android.icu.util.LocaleNative.setDefaultNative`
(3 calls). `java.lang.Character`'s 5 natives are ICU-*backed* (type,
directionality, case) but are libcore natives, not ICU4J. **No** `Collator`/
`BreakIterator`/`PatternNative` natives appear: ICU4J's rule-based break iterator
(`android.icu.text.RuleBasedBreakIterator`, 28+28 methods in interaction) is
*Java* and reads ICU data files (G-1).

Recommendation: **do not embed ICU4C.** Implement (a) a Rust converter registry
for UTF-8/UTF-16/Latin-1/US-ASCII (covers every measured conversion) with an
explicit `UNSUPPORTED_CHARSET` for the rest, (b) `Character` tables generated
from Unicode data (or reuse `unicode-*` crates + a generated table for
`getTypeImpl`/`getDirectionalityImpl`/case), and (c) ship/point at the real
`icudt*.dat` for the *Java* ICU4J path (no Rust work). Evaluate `icu4x` only if a
measured path needs a real converter/locale service — at 15 enters at launch and
6 in interaction, porting ICU4C is unjustified.

### 8.5 Test strategy per module

**Universal pattern: replay the recorded traces.** The ART traces give, per
native, the exact class/method/signature, call counts and first order. Build
`tools/oracle/native-diff` (backlog T-11) that (1) runs a scenario under ACOS
with a *native call recorder* (same TSV shape), (2) diffs against the ART TSV for
(key, count, order-window), and (3) fails on any native the ART trace does not
contain ("unexpected work") as well as any missing one. This is the differential
gate for every module.

| module | unit tests | differential test |
|---|---|---|
| core.lang | JVM-vs-ACOS on the 20 dexprogs (`jvm-reference`, weak) | `methods_println.tsv` replay: 21 natives, 5,996 invocations, exit/stdout byte-identical |
| core.thread/vmrt | GC-stress suite; Unsafe offset table vs a generated golden file | `Unsafe` natives: 13 distinct, 50+11 enters in interaction — assert the same offsets are used |
| os.parcel | golden parcel vectors (encode→decode→encode fixpoint) + `enforceInterface` mismatch throws | run the real `$Stub$Proxy` against the Rust transport and assert the decoded arguments equal the Java-side getters (e.g. `LayoutParams.width==1080`) |
| res | arsc/axml parser vs `aapt2 dump` (already differential-tested in `acos-apk`) | `XmlBlock.nativeNext` count 226 ± order; `TypedArray` values for `android:attr/textAppearanceLarge` etc. |
| render.* | `acos-render` scene tests + PNG hash for a fixed scene | first-frame pixel diff vs `fixtures/reference/hello-views/.../screenshot_initial.png` with a stated tolerance; text bounds vs `ui_initial.json` (**exact** [42,317][231,368] etc.) |
| text.* | shaping/metrics unit tests | same bounds test above — text bounds are the strictest available oracle |
| view.input | synthetic `MotionEvent` stream | replay the reference tap coordinates (540,427 / 157,549) and assert `clicks: N` and the `EditText` text |
| view.surface/vsync | window state-machine tests (pure Rust) | frame_seq monotonic, one frame per input, zero wakeups when idle (`/proc/<pid>/stat` utime+stime delta over 1 s == 0) |

---

## 9. Risk register (top 10) with falsification tests

| # | risk | why it could sink the design | cheap falsifying experiment | cost |
|---|---|---|---|---|
| **R-01** | **Framework `<clinit>`/class-load time dwarfs the interpreter** (ACOS must do from scratch what the boot image hid) | if loading 1,128 classes + `<clinit>` costs seconds, cold start is unreachable and the snapshot becomes mandatory, not optional | boot ACOS with *only* core-oj and time `Class.forName`+`<clinit>` for the 1,128 executed classes (DERIVED list from `trace_*.json`); compare to 466 ms | < 1 day |
| **R-02** | **Parcel/AIDL incompatibility** (one wrong field order → exception or silent wrong value) | every service reply rides it; a silent wrong `DisplayInfo` changes layout everywhere | run the real `IWindowSession$Stub$Proxy.addToDisplayAsUser` against the Rust transport in a unit test with a *recording* reply parcel; then assert the Java side's `mAttachInfo`/`mWinFrame` values equal the reference (`display.txt`) | 2 days |
| **R-03** | **`HardwareRenderer`/BLAST state machine is deeper than the 32 natives suggest** (render thread, buffer allocation, frame timelines) | trades one fidelity hole (no real HWUI) for a second (a different frame lifecycle) | prototype: drive `ThreadedRenderer.draw` with a Rust `nSyncAndDrawFrame` that returns immediately and see whether the framework accepts a second frame (it calls `nNotifyFramePending`/`nGetRenderThreadTid`) | 2–3 days |
| **R-04** | **Text metrics/bounds mismatch** (Minikin vs rustybuzz/harfbuzz, hinting, hyphenation, fallback) | `docs/V1.md` 1.4/1.9 require exact bounds; text is 3,264 method calls at launch and 46,813 in interaction | compare `Paint.nGetCharArrayBounds`/`LineBreaker` output for the 5 reference strings against `ui_initial.json` bounds (exact pixels) — cheap, and fails loudly if shaping differs | 1–2 days |
| **R-05** | **Thread/Looper model mismatch** (`HandlerThread`s, render thread, blocking natives, snapshot forbidding threads) | a hidden `HandlerThread` wait turns into a deadlock; a snapshot with threads is unusable | instrument the VM: log every `Thread.start` + `Looper.loop` and run the launch; then assert the same thread set as `/proc/<pid>/task` on ART (GAP G-5) | 1 day |
| **R-06** | **ART-specific intrinsics with no portable semantics** (`Unsafe` pointer arithmetic, `VMRuntime.addressOf`, non-movable arrays, identity hash, `Reference`/GC interplay) | AndroidX/Compose depend on them; a wrong `Unsafe` offset is memory corruption in *our* heap | implement `Unsafe` on offsets only and run the interaction trace path that uses it 13 ways (`getReferenceVolatile` 50, `compareAndSetInt` 11); GC-stress must survive | 2 days |
| **R-07** | **Resource/theme loading** (`AssetManager`/`ApkAssets`/`StringBlock`/theme styles across 34 MB `framework-res.apk`) | inflation of the decor view + `android:` theme attrs fails → no Activity at all | parse `framework-res.apk` with `acos-apk` and dump the resolved `android:attr/colorBackground`, `textAppearanceLarge`; compare with the reference screenshot's colours | 2 days |
| **R-08** | **Hidden-API / reflection-heavy paths** (`Class.getDeclaredMethodInternal`, `getPublicFieldRecursive`, `PropertyValuesHolder.nCallFloatMethod`) | the framework itself uses reflection on hidden members; a missing member changes behaviour silently | run the launch with a "reflection audit" that logs every reflective lookup and fails on a miss; the ART trace lists the exact call counts (6 `getPublicFieldRecursive`, 6 `getDeclaredMethodInternal`) | 1 day |
| **R-09** | **Interpreter throughput too low without JIT** (no JIT per `docs/V1.md` scope) | 3.18 M units (launch) at 10 M units/s = 318 ms — already the whole budget | microbenchmark the existing interpreter on `methods_hello_views_launch` workloads at 20/50/100/200 M units/s (§9.1) | 1 day |
| **R-10** | **Snapshot/restore correctness** (statics, identity hash, fds, threads, determinism) | ADR-0010 gates the whole cold-start/reset story; a subtle miss is a hard-to-find behavioural difference | implement snapshot for **core only** (println), restore 100×, run the 20 dexprogs; then add one framework class at a time and re-run | 3–5 days |

### 9.1 Interpreter workload (DERIVED, honest bounds)

Method: join each trace row against the DEX `insns size` of that method
(`dex_method_sizes.py`, 288,161 methods from 14 boot dex files; 6,436/6,639
non-native launch methods matched = 97 %), then
`Σ enters × insns_size`. This is an **upper bound** (a method body's instructions
are not all executed; branches skip) and *excludes* native bodies and
`<clinit>`/class loading.

| phase | invocations | native invocations | est. 16-bit units (upper bound) | units/invocation |
|---|---:|---:|---:|---:|
| println | 5,996 | 2,130 | 95,317 | 15.9 |
| launch → first frame | 152,321 | 34,262 | **3,182,748** | 20.9 |
| interaction (4 actions) | 290,799 | 33,390 | **7,061,529** | 24.3 |

Time at different interpreter rates (this table is the ADR-0010 gate #1 input):

| rate | println | launch (upper) | launch @ 40 % body coverage (INFERRED) | interaction (upper) |
|---|---:|---:|---:|---:|
| 50 M units/s | 1.9 ms | 64 ms | 25 ms | 141 ms |
| 100 M units/s | 1.0 ms | 32 ms | 13 ms | 71 ms |
| 200 M units/s | 0.5 ms | 16 ms | 6 ms | 35 ms |

Real ART for comparison (MEASURED): 466 ms cold, ~180 ms to first *draw*
(tracing-inflated), warm 17 ms. **Reading**: at ≥50 M units/s the *interpreter* is
not the bottleneck — the budget is consumed by class loading/`<clinit>`,
resource/XML/arsc work, font/text init, native graphics, and process/snapshot
setup. That is why R-01/R-07/R-03 outrank R-09, and why the snapshot is the
central perf lever rather than a faster interpreter.

### 9.2 Assumptions (explicit) and unknowns

**Assumptions (A)**

- A1 The measured hello-views launch/in interaction traces are representative of
  the V1 fixture set; hello-jni's 379 natives are a subset modulo its 6 app
  natives (MEASURED).
- A2 Answering `IInputMethodManager.startInputOrWindowGainedFocus` with "no IME"
  is acceptable for V1 because input is injected by the Rust dispatcher and the
  reference capture shows no IME surface (MEASURED: `ui_initial.json` has no IME
  window; `display.txt` `ImeContainer` empty, `mIsImeShowing=false`).
- A3 The app has exactly one process, one display, uid 10192, package
  `dev.acos.fixtures.helloviews` (MEASURED).
- A4 `GraphicsStats` can hand out a memfd that nobody reads (no frame-metric
  assertions in V1).
- A5 The 5/4 `IContentProvider.call` transactions are Settings reads, not
  arbitrary providers (INFERRED from `Settings$NameValueCache` 14 calls and
  `getContentProvider` 1 call; falsify by logging the method/arg strings at the
  Rust provider).
- A6 `DisplayInfo` values beyond the ones in `display.txt` (xdpi/ydpi, HDR
  capabilities, supported color modes) do not affect the reference bounds
  (INFERRED; falsified by a bounds mismatch in the first frame test).

**Unknowns (U) / GAPs**

- U1 (G-1) ICU data file location/format for ICU4J's Java-side break iterator.
- U2 (G-2) the actual system property keys read (49 `native_get_long` calls).
- U3 (G-3) the 3 `hasSystemFeature` names.
- U4 (G-5) the thread inventory of a real hello-views process.
- U5 (G-6) the true from-scratch `<clinit>` count/order for the executed set.
- U6 The exact `DisplayInfo`/`PackageInfo` field sets written by API 34 (codegen
  will answer; no API-34 sources are installed — only android-36).
- U7 Whether `HardwareRenderer`'s render-thread contract can be satisfied
  without a real render thread (R-03).
- U8 Whether `Parcel`'s `String8` header word is `(len<<1)|1` on this build
  (verify against the real writer before implementing — it is a one-line test).
- U9 Interaction latency (tap → frame) was never measured per action; the trace
  spans 3.2 s of scripted waits.

---

## 10. What is *not* covered here (scope honesty)

- No multi-process/app-isolation design (out of V1 scope).
- No IME/bind to a real InputMethodService (V1 declares "no IME").
- No audio (`IAudioService.playSoundEffect` is accepted and dropped — declared).
- No networking (`IConnectivityManager` returns "no proxy"; no sockets on path).
- No a11y tree beyond `addClient` disabled.
- No GPU: `acos-render` is the software renderer (ADR-0011).
- The 5,313 non-measured boot-classpath natives are **not** in this plan; the
  plan covers exactly the measured 520 and will grow with the trace corpus.

---

## 11. Deliverables in this directory

| file | content |
|---|---|
| `REPORT.md` | this document |
| `services.tsv` | 108 rows: every measured outgoing transaction (83 method rows over 26 interfaces, incl. the unlisted `IContentProvider`) + 25 incoming rows over 11 interfaces; owner, statefulness, reply design, consumer, risk |
| `natives_plan.tsv` | 520 rows: every native in the launch/interaction/println traces with module, complexity, phase counts, first order, notes |
| `gen_services.py` | generator for `services.tsv` (counts injected from the trace TSVs) |
| `gen_natives_plan.py` | generator for `natives_plan.tsv` (curated module map + per-method overrides) |
| `dex_method_sizes.py` | `dexdump` → (class, method, proto, insns size) extractor used for §9.1; writes 288,161 rows to stdout in ~2 s (**not committed** — 28 MB; regenerate with the command in §12) |
| `BACKLOG.md` | dependency-ordered, subagent-sized implementation tasks with verifying tests |

## 12. Reproduction

```bash
# regenerate the machine-readable deliverables (no emulator, no network)
cd ~/Projects/acos
python3 research/service-layer/gen_services.py
python3 research/service-layer/gen_natives_plan.py

# re-derive the instruction-size table (if the DEX cache changes)
python3 research/service-layer/dex_method_sizes.py \
  .cache/boot/android-34/dex/framework/classes{2,3,4,5,}.dex \
  > research/service-layer/dex_method_sizes.tsv

# measurement gaps (do NOT run casually: needs the oracle emulator, ADR-0015)
tools/oracle/emulator.sh start
# ... then the adb commands in section 5.2 (G-1, G-2, G-5)
tools/oracle/emulator.sh stop
```
