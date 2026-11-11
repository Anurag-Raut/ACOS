# P4b — `Application.onCreate` without instrumentation, and the ClientTransaction that reaches `Activity.attach`

**Status: partial, stopped at the hard 60-minute checkpoint (kill/OOM context: this is a
resume of the P4b agent killed by the host OOM, so the checkpoint was already spent once).
Nothing committed. Two of the three acceptance bars are met; the third is one
`IContentProvider` wall away and is named exactly below.**

* **`Application.onCreate` runs in the real `ActivityThread`, with `instrumentationName == null`
  and no class from the APK or the guest** — the framework itself does
  `mInstrumentation = new Instrumentation(); basicInit(this)` then
  `callApplicationOnCreate(app)` (`ActivityThread.java:7325–7327`, `:7398`).
  `--until application-created` is **`outcome: ok`**, wall **106–131 ms**.
* **The real AMS launch step works**: one framework-marshalled
  `ClientTransaction{LaunchActivityItem, ResumeActivityItem}` is delivered to the real
  `ApplicationThread` as `IApplicationThread.scheduleTransaction` (**code 54**) over the
  binder peer (2108 bytes, `txn=Some(2108)/delivered=true`), the app thread dispatches
  `H.EXECUTE_TRANSACTION` (**159**), `LaunchActivityItem.execute` builds the
  `ActivityClientRecord`, and **`performLaunchActivity` starts the APK's `MainActivity`**:
  `Activity.attach` is entered, `new PhoneWindow(...)` is constructed from the app's real
  `ActivityInfo`. `Activity.onCreate` does **not** complete (blocker §4).
* **Frame unchanged**: `render-app` on `counter.apk` is byte-identical to P3c
  (`md5 9ec17f055843a2b9d18078ec595e9300`), SSIM **0.99351**, tree **12/12**, structural 8/8.
* **R1's servicemanager spin is gone**: `servicemanager.ready=true` is now in the property
  table, so `libbinder`'s `defaultServiceManager()` wait (`IServiceManager.cpp:152–157`) ends
  on the first check and the 1.5 M lines/s retry loop cannot start.

```bash
# Application.onCreate via the real framework path (instrumentationName == null)
tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art launch \
    --apk fixtures/build/apps/counter.apk --until application-created \
    --out results/art-runtime/p4b/app
# outcome=ok wall=106ms application_created=true txn=None/0 bind=1324B calls=23 unsupported=2

# + the AMS launch step (ClientTransaction -> Activity.attach)
tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art launch \
    --apk fixtures/build/apps/counter.apk --until activity-resumed \
    --out results/art-runtime/p4b/activity
# outcome=crash code=NO_ACTIVITY_RESUMED txn=Some(2108)/delivered=true calls=34 (§4)
```

## 0. Acceptance

| requirement | result |
|---|---|
| `Application.onCreate` of the `dev.acos.fixtures.counter` APK runs in the real `ActivityThread` | **PASS** — `mInitialApplication=android.app.Application`, its base context is `dev.acos.fixtures.counter` / `/data/app/counter.apk`; `BIND_APPLICATION` (**110**) dispatched **and finished** on the real main `Looper` (§1) |
| `instrumentationName == null`; **the framework** creates the `Instrumentation` | **PASS** — bind request writes `null`; marker records `instrumentation=android.app.Instrumentation` (a plain framework instance, not a subclass); `LaunchMain$Instr` and the split APK are **deleted** (§2) |
| real `LaunchActivityItem`/`ResumeActivityItem` via `ClientTransaction` delivered over the binder peer (`ApplicationThread.scheduleTransaction`) | **PASS** — 2108-byte parcel written by `ClientTransaction.writeToParcel`, stashed on `acos.launch`, delivered as `F_TXN` code 54 to the `IApplicationThread` wire handle; guest log shows `H` message **159** dispatched (§3) |
| `Activity.onCreate` / `onResume` | **FAIL (blocked)** — `Activity.attach` → `new PhoneWindow` → `Settings.Global.getInt` → `IContentProvider.call` blocks; no `AcosCounter` log line, `mActivities[null].activity == null` (§4) |
| window add/relayout answered by a WMS stand-in, frame captured | **NOT REACHED** — the exact requirement list is in §5 |
| frame SSIM vs oracle ≥ 0.99 unchanged | **PASS** — 0.99351 (P3c 0.99351), md5 identical, tree 12/12 |
| `cargo test -p acos-art -p acos-binder-peer` | **PASS at the time of the runs** — 39 + 6 and 3 + 12, 0 failed. **Re-check before committing:** a *concurrent* workstream's uncommitted edit to `crates/acos-services/src/parcelable.rs` (`FieldOp::CountedNested`/`FieldOp::Cutout` arms missing, `M` in `git status`, touched by another agent — **not** by P4b, and deliberately not fixed here) makes `cargo test -p acos-art` fail to compile as of the end of this task. The already-built `target/debug/acos-art` still runs (the `outcome: ok` run in §1 was made with it) |
| `servicemanager.ready` answered (R1 spin) | **PASS** — property `servicemanager.ready=true`; no `Waited for servicemanager.ready` line in either run's logd |
| every ART command under `tools/guarded.sh`, one ART process, RAM < 90 %, no sudo, no commit | **PASS** — all runs `--mem 4G --time 300`; host 22–23 / 60 GB used (38 %); one ART process at a time; scratch under `.cache/p4b/` |

## 1. What is real in the `application-created` run

```text
ACOSLAUNCH MARK android-runtime ready rc=1
ACOSLAUNCH MARK binder-looper started
ACOSLAUNCH MARK bind-request-marshalled appInfo=(dev.acos.fixtures.counter sourceDir=/data/app/counter.apk uid=1000) bytes=1324
ACOSLAUNCH MARK attach-returned startSeq=…
ACOSLAUNCH TRACE >>>>> Dispatching to Handler (android.app.ActivityThread$H) {a352f2b} null: 110
ACOSLAUNCH TRACE <<<<< Finished to Handler (android.app.ActivityThread$H) {a352f2b} null
ACOSLAUNCH MARK application-created package=dev.acos.fixtures.counter application=android.app.Application
                    base=dev.acos.fixtures.counter//data/app/counter.apk instrumentation=android.app.Instrumentation
ACOSLAUNCH MARK bind-application-dispatch-finished=true
```

`application-created.marker`, verbatim:

```
package=dev.acos.fixtures.counter
application=android.app.Application
application_classloader_is_apk=true
application_base=dev.acos.fixtures.counter//data/app/counter.apk
instrumentation=android.app.Instrumentation
instrumentation_source=framework (bind request instrumentationName=null)
sourceDir=/data/app/counter.apk
```

The trace lines are the framework's own `Looper.setMessageLogging` printer — the facility
AMS-side tooling uses. `>>>>> … 110` / `<<<<< Finished` on the **main** looper means the
real `H.handleMessage` ran `handleBindApplication` to completion; the only application code
in that span is `Instrumentation.callApplicationOnCreate(app)` →
`Application.onCreate()`. The fixture declares **no** `android:name` on `<application>`, so
`onCreate` is `android.app.Application.onCreate` (an empty body): the proof that it was
*called* is that the dispatch completed, the object exists, and `mInstrumentation` is the
framework's own — not a side effect of fixture code. Stated plainly so it is not overread.

## 2. What was removed (the P4a blocker is gone, not worked around)

| P4a | P4b |
|---|---|
| bind request named `dev.acos.art.LaunchMain$Instr` as `instrumentationName` | writes `null` (`p.writeTypedObject(null, 0)`) — the framework's `else` branch (`ActivityThread.java:7325`) |
| guest zipped its own dex to `/data/app/launch-split.apk` and set `splitSourceDirs`/`splitNames` | **deleted** (`writeSplitApk`, `GUEST_DEX`, `SPLIT_APK`, both `ApplicationInfo` split fields) |
| `LaunchMain$Instr extends Instrumentation` wrote the marker | **deleted**; the marker is written by the guest watcher from real framework state (`ActivityThread.mInitialApplication`) |
| bind request 1492 bytes (split in `ApplicationInfo`) | 1324 bytes |

## 3. The ClientTransaction (real AMS step, real parcel writer)

`LaunchMain.buildScheduleTransactionRequest` (guest) does what `ActivityStarter` →
`ClientLifecycleManager.scheduleTransaction` does in system_server:

1. `ActivityInfo` for the APK's launcher activity (`aapt2 dump badging` →
   `launchable-activity`, resolved host-side in `launch.rs::apk_launcher_activity`):
   `applicationInfo` = the same `ApplicationInfo` as the bind request, `name`,
   `packageName`, `taskAffinity`, `launchMode=0`, `theme=0` (platform default),
   `flags=0` (**no `FLAG_HARDWARE_ACCELERATED`**, so the app takes the software
   `ViewRootImpl` path — the only drawing route that has any chance without SF), `enabled`.
2. `ClientTransaction.obtain(IApplicationThread.Stub.asInterface(mAppThread), token)`.
3. `LaunchActivityItem.obtain(...)` and `ResumeActivityItem.obtain(...)`, both called with
   arguments matched to **this image's** parameter list, read from the method at runtime
   (`obtain-shape` marker). This matters: the image is API 34 `UE1A.230829.050`, whose
   `LaunchActivityItem.obtain` is
   `(Intent, int, ActivityInfo, Configuration, Configuration, int, String, IVoiceInteractor,
   int, Bundle, PersistableBundle, List, List, ActivityOptions, Z, ProfilerInfo, IBinder,
   IActivityClientController, IBinder, Z, IBinder)` — **no `ActivityWindowInfo`, no
   `SceneTransitionInfo`, `ActivityOptions` instead, and no activity token argument** (the
   token is carried by `ClientTransaction.mActivityToken`; the `android-14.0.0_r75` tree in
   `.cache/aosp` is *newer* than the image and was not used as the signature source).
   Evidence: `dexdump` of the image's `framework/classes.dex`, and the `obtain-shape` line.
4. Items placed in the two private fields `TransactionExecutor` actually reads
   (`mActivityCallbacks`, `mLifecycleStateRequest`) — this image has no public
   `addTransactionItem`/`mTransactionItems`.
5. `Parcel.writeInterfaceToken("android.app.IApplicationThread")` +
   `Parcel.writeTypedObject(transaction, 0)` — the exact AIDL argument shape — then
   `pushToPeer(3, bytes)`.

Peer (`crates/acos-binder-peer/src/peer.rs`): `LAUNCH_PUSH_TRANSACTION = 3` stashes the
bytes; `take_launch_delivery` now has **two** stages — `bindApplication` (code 5) once
`attachApplication` named the `IApplicationThread`, then `scheduleTransaction`
(**code 54**, from the image's `IApplicationThread.aidl` order: 63 oneway methods,
`scheduleTransaction` is #54) to the same wire handle. Delivery is a `F_TXN` frame the
userspace driver turns into a `BR_TRANSACTION`, i.e. the identical P2f reverse path that
already carries `bindApplication`.

Guest-side result: `>>>>> Dispatching … android.app.ActivityThread$H … : 159`, and
`performLaunchActivity` starts running (§4).

### The one deliberate stand-in on this path

`Parcel.marshall()` refuses a parcel that contains binder objects ("Tried to marshall a
Parcel that contains objects (binders or FDs)") — the ACOS transport is a byte pipe, not a
kernel binder transaction. The **activity token and the assist/shareable/task-fragment
tokens therefore travel as `null`**. Their identity is only meaningful to AMS; in ACOS the
AMS stand-in *is* the peer, and `ActivityThread` keys `mActivities` by it
(`mActivities[null]` holds the record, visible in the diagnostic dump). Everything else in
the transaction — `Intent`, `ActivityInfo`, `Configuration`, `ActivityOptions`, the item
objects, the parcel layout — is the framework's own.

## 4. The blocker (exact): `Settings.Global` → `IContentProvider.call` blocks the main thread

```
ACOSLAUNCH MARK   token=null info=ActivityInfo{92355f9 dev.acos.fixtures.counter.MainActivity} activity=null
ACOSLAUNCH THREAD main RUNNABLE
ACOSLAUNCH   at android.os.BinderProxy.transactNative(Native Method)
ACOSLAUNCH   at android.os.BinderProxy.transact(BinderProxy.java:584)
ACOSLAUNCH   at android.content.ContentProviderProxy.call(ContentProviderNative.java:762)
ACOSLAUNCH   at android.provider.Settings$NameValueCache.getStringForUser(Settings.java:3331)
ACOSLAUNCH   at android.provider.Settings$Global.getStringForUser(Settings.java:16836)
ACOSLAUNCH   at android.provider.Settings$Global.getInt(Settings.java:17036)
ACOSLAUNCH   at com.android.internal.policy.PhoneWindow.<init>(PhoneWindow.java:361)
ACOSLAUNCH   at com.android.internal.policy.PhoneWindow.<init>(PhoneWindow.java:371)
ACOSLAUNCH   at android.app.Activity.attach(Activity.java:8491)
ACOSLAUNCH   at android.app.ActivityThread.performLaunchActivity(ActivityThread.java:3733)
ACOSLAUNCH   at android.app.ActivityThread.handleLaunchActivity(ActivityThread.java:3922)
```

So the launch is **one call into `Activity.attach` past** the P2a/P3c `Settings` wall — but
this time from the real path, and it is a **hang, not an exception**: the last two service
transactions of the run are

| # | descriptor | code | method |
|---|---|---|---|
| 33 | `android.app.IActivityManager` | 30 | `getContentProvider` |
| 34 | `android.content.IContentProvider` | 21 | `call` |

`getContentProvider("settings")` is answered with a `ContentProviderHolder` whose binder is
a handle **no thread serves**, so `ContentProviderProxy.call` blocks in `transactNative`
forever (the run ends by the harness' own 12 s deadline, `NO_ACTIVITY_RESUMED`, never a fake
success). This is the same `Settings.Config`/`Settings.Global` gap P2a §4 and P3c §4 record
for `hello-views`, now with the real caller named: `PhoneWindow.<init>` reads
`Settings.Global` **before** `Activity.onCreate` can run.

The fix is one of (both cheap, none done — checkpoint):

1. **Serve the `settings` provider from the peer** (`acos-services`-owned): answer
   `getContentProvider("settings")` with a *peer* node that implements
   `IContentProvider.call` for the `Settings.Global`/`Settings.System`/`Settings.Secure`
   argument shape and returns the platform defaults (`device_provisioned=1`,
   `user_setup_complete=1`, animation scales, …). P3c already prototyped a local
   `IContentProvider` for `Settings.Config` in `guest/java/…/AppMain.java:681–736`; the same
   table belongs in `acos-services` so both engines share it.
2. Or answer with a null holder *and* a `RemoteException` reply for the provider call so
   `Settings$NameValueCache` takes its `catch` branch and `getInt` returns the default —
   cheaper, but leaves the provider genuinely unsupported (a structured
   `UNSUPPORTED_ANDROID_API` rather than a hang, which is the policy either way).

Either way the **hang must become a structured failure** even if the value is not served:
a blocked main thread is the one outcome ACOS's "never fake, always structured" rule
cannot express.

## 5. What `ViewRootImpl` will need next (asked explicitly)

Nothing below is speculative: it is the code path `handleResumeActivity` →
`WindowManagerGlobal.addView` → `ViewRootImpl.setView` takes in this image, with the AOSP
reference lines.

| # | requirement | where it is demanded | what ACOS has / needs |
|---|---|---|---|
| 1 | `IWindowSession.addToDisplayAsUser(IWindow, LayoutParams, int, int, int, int[], InputChannel, Insets, Insets, Rect, float[])` returning `int res` + out `InputChannel`/insets | `ViewRootImpl.java:1476` | an `IWindowSession` node in the peer (partially exists: `acos-wm` answers scalar `IWindowManager` calls — `getCurrentAnimatorScale` (42), `useBLAST` (5) already arrived and were answered). Needs: the full `addToDisplayAsUser` args+reply codec (InsetsState/InsetsSourceControl/`InputChannel`), `relayout(…, Surface out, SurfaceControl out)`, `remove`, `finishDrawing`, `setInTouchMode`, `setTransparentRegion`, `getDisplayFrame`, `getWindowId`, `onRectangleOnScreenRequested` |
| 2 | a `SurfaceControl`/`Surface` for the window | `ViewRootImpl.java:2523` (`mSurface.transferFrom(blastSurface)`, built from the `relayout` reply), `:5365` (`mSurface.lockCanvas(dirty)` for the software path) | **nothing**: this needs either a real SF layer (BLAST + gralloc + `/dev/binder`) or an ACOS-owned `IGraphicBufferProducer`/`ANativeWindow` whose buffers are shmem the shim can map. Cheapest honest rung: a `libui`-level `ANativeWindow` + `GraphicBuffer` over an ashmem bitmap, so `lockCanvas` hands `drawSoftware` a real `Canvas` (that is the "software Surface path" the task asks about, and it reuses the P3c/P2e draw path one-for-one). Rung 0 (zero Surface work): `relayout` returns a null SurfaceControl, `ViewRootImpl` logs *OutOfResources* / "Surface not valid" and skips drawing — `onCreate`/`onResume` still complete, the frame stays the `render-app` one |
| 3 | vsync: `DisplayEventReceiver` → `ComposerServiceAIDL::getComposerService()` → `ISurfaceComposer.createDisplayEventConnection` → `BitTube` | `native/libs/gui/DisplayEventReceiver.cpp:36–46`; `ViewRootImpl.java:1169` (`Choreographer.getInstance()`), `:2693` (`postCallback(CALLBACK_TRAVERSAL…)`) | **nothing**: `Choreographer` is thread-local and constructed in the `ViewRootImpl` ctor, so it fails **before** any traversal. R1 already recorded the same abort from the session path (`AnimationHandler.getProvider → Choreographer`). Needs a peer node for the native servicemanager name `SurfaceFlinger` whose `createDisplayEventConnection` returns a `BitTube` (socketpair) fd, plus a timer that writes vsync events into it — otherwise no traversal ever runs |
| 4 | `IActivityClientController` (for `handleLaunchActivity → reportSizeConfigurations`) | `ActivityThread.java:4185` | **already answered** — `IActivityTaskManager.getActivityClientController` (18) and `IActivityClientController.getDisplayId` (21) are in the surface and replied |
| 5 | `InputChannel` (out of `addToDisplay`) | `ViewRootImpl.java:1476` | the driver already passes fds (P2f), so an empty channel backed by a `socketpair` is enough for "no input yet" |

Order of work for the next agent, cheapest first: **(a)** the `settings` provider (§4) — this
alone gives `Activity.onCreate` + `onStart` + `onResume` and the fixture's own
`AcosCounter: onCreate/onStart/onResume` log lines; **(b)** `addToDisplayAsUser`/`relayout`
with a null SurfaceControl (§5.2 rung 0) — this gets `performTraversals` past `relayout`
without drawing; **(c)** the vsync peer node (§5.3); **(d)** the shmem `ANativeWindow`
(§5.2 rung 1) if the pixels must come from the app's own `Canvas`.

## 6. R1's spin: fixed at the source

`libbinder`'s `defaultServiceManager()` waits for a **property**, not a binder, before it
ever talks to the context object:

```cpp
// .cache/aosp/native/libs/binder/IServiceManager.cpp:152
while (!WaitForProperty("servicemanager.ready", "true", 1s)) {
    ALOGE("Waited for servicemanager.ready for a second, waiting another...");
}
```

ACOS runs no servicemanager *process*, but its service manager (the `IServiceManager` node
answered by `acos-binder-peer`/`ServiceHost`) is up before any guest starts — so
`servicemanager.ready=true` is a true statement about this system, and it is now part of
`Props::android34()` (`crates/acos-art/src/props.rs`). Effect: neither run logs
`Waited for servicemanager.ready`, and the R1 log flood (1.5 M entries/s → the OOM) has no
trigger left on this path. The R1 caps stay in place underneath as defence in depth
(`logd` 50 000 entries, stream 8 MiB, `ACOS_ART_RSS_MAX` 2 GiB).

## 7. Real vs stand-in (one table, no overclaiming)

| piece | verdict |
|---|---|
| `Looper.prepareMainLooper`, `new ActivityThread()`, `attach(false, seq)` → `IActivityManager.attachApplication` | **real framework + real host transaction** |
| `IApplicationThread.bindApplication` request | **real**: marshalled by the framework's `Parcel.writeTypedObject` calls, delivered as a `BR_TRANSACTION` to the real `ApplicationThread` |
| `handleBindApplication`, `LoadedApk`, `ContextImpl`, `nativeloader` class loader from `counter.apk`, `makeApplicationInner`, `Application` instance | **real framework**, services answered by the Rust `ServiceHost` |
| `Instrumentation` | **real framework instance, created by the framework** (`instrumentationName == null`); no subclass anywhere |
| `Application.onCreate` | **real call**, `android.app.Application.onCreate` (empty body in the platform): proved by the `BIND_APPLICATION` (110) dispatch completing on the main looper + `mInitialApplication` |
| `ClientTransaction` / `LaunchActivityItem` / `ResumeActivityItem` | **real framework objects and real parcel bytes** (`ClientTransaction.writeToParcel`) |
| `IApplicationThread.scheduleTransaction` transport | ACOS: `acos.launch` node → peer → `F_TXN` code 54 → userspace binder driver → `BR_TRANSACTION`. **The same reverse path as P2f/P4a, not a kernel binder** |
| activity token & the three system_server binder tokens inside `ClientTransaction` | **null** (stand-in): `Parcel.marshall()` cannot carry binder objects; token identity only matters to AMS, whose stand-in is the peer |
| `getDisplayInfo` / `getApplicationInfo` / `getInstrumentationInfoAsUser` replies | **framework-marshalled reply templates replayed by the peer** — the transaction is real, the parcel writer is the framework's, the *values* are the display/app this launch serves (P4a §7.2 seam, still open; the instrumentation template is now dead weight and can go) |
| `IContentProvider` (`settings`) | **missing** — `getContentProvider` answers a holder whose binder nobody serves → the main thread blocks (§4). This is the wall |
| `IWindowSession`, `Surface(Control)`, `Choreographer`/vsync | **missing** — §5; nothing pretends otherwise |
| frame | from `render-app` (P3c chrome path), unchanged |

## 8. Files touched (all uncommitted)

| path | change |
|---|---|
| `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` | `instrumentationName=null`; split APK + `Instr` removed; looper trace printer; `buildScheduleTransactionRequest` + image-shaped `obtain`/`obtainResume`; watcher that acts as the AMS side (waits for `mInitialApplication`, writes markers, pushes the transaction, waits for `Activity.mResumed`); `quit()` ends the one-shot process instead of quitting the quit-disabled main queue |
| `crates/acos-art/src/launch.rs` | `--until {application-created, activity-resumed}`; launcher activity from `aapt2 dump badging`; extra guest argv; `activity`, `activity_marker`, `looper_trace` in `LaunchResult`; `NO_ACTIVITY_RESUMED`; paired 110/159 trace extraction |
| `crates/acos-art/src/props.rs` | `servicemanager.ready=true` (§6) |
| `crates/acos-binder-peer/src/peer.rs` | `LAUNCH_PUSH_TRANSACTION=3`, `SCHEDULE_TRANSACTION_CODE=54`, `launch_txn*` state + accessors, two-stage `take_launch_delivery` |
| `results/art-runtime/p4b/{app,activity}/` | markers, `launch.json`/diagnostics, `unsupported.jsonl`; `p4b/counter/{frame.png,tree.json}` + `p4b/compare.json` |

**No `crates/acos-services` edit was made** (not even additive; the `parcelable.rs` `M` in
`git status` is another workstream's mid-flight edit, and it is why a fresh
`cargo test -p acos-art` does not compile at the moment — see §0): the `settings` provider fix
in §4 is the next owner's, and the `notifyGraphicsEnvironmentSetup` stub P4a added is
untouched. `crates/acos-runtime` untouched. Guest report: 39 lib + 6 integration tests in
`acos-art`, 3 + 12 in `acos-binder-peer`, all green.

## 9. Structured unsupported records (never faked), this run

| descriptor | code | method | cause |
|---|---|---|---|
| `android.content.pm.IPackageManager` | 106 | `notifyDexLoad` | AIDL arg table stale (API 34 takes a `Map<String,String>` class-loader context) — unchanged from P4a |
| `android.app.IActivityManager` | 101 | `handleApplicationWtf` | no codec for `ApplicationErrorReport$ParcelableCrashInfo` (it is reporting the `netstats` service miss) |

Both keep `"descriptor":"<unknown>"` because the peer re-peeks a consumed parcel (P4a §5,
open).

## 10. Reproduce the diagnostics

```bash
# every framework message with its handler and `what` (110/159 are the two that matter)
tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art launch \
    --apk fixtures/build/apps/counter.apk --until activity-resumed 2>&1 | grep ACOSLAUNCH

# peer side: both reverse deliveries, the reply templates, the unsupported records
… 2>&1 | grep acos-binder-peer
#   delivering bindApplication (1324 bytes, code 5) to java binder wire=1
#   delivering ClientTransaction (2108 bytes, IApplicationThread.scheduleTransaction code 54) to java binder wire=1

# the image's real ClientTransaction/LaunchActivityItem shapes (not the r75 source)
~/Android/Sdk/build-tools/34.0.0/dexdump -d -f .cache/boot/android-34/dex/framework/classes.dex \
  | grep -A6 "name          : 'obtain'"   # inside the LaunchActivityItem class block
```
