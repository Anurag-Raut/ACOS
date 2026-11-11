# P4a — the real `ActivityThread.attach` → `bindApplication` → `Application` path

**Status: partial (hard 60-minute checkpoint).** The framework startup is now real
almost all the way: `Looper.prepareMainLooper()` → `new ActivityThread()` →
`attach(false, startSeq)` → the Rust `ServiceHost` answers
`IActivityManager.attachApplication` → the peer delivers the real
`IApplicationThread.bindApplication` (code 5) through the P2f reverse path →
`ActivityThread.handleBindApplication` runs the stock code, builds the app's
`LoadedApk` **from `counter.apk` with the real `nativeloader` class loader**,
serves `getApplicationInfo` from the real APK, and reaches
`initInstrumentation(...)`. It then **stops on one blocker** (§4): the launch
instrumentation class is not loadable from the app's class loader, so
`Application.onCreate` was **not** reached and `--until application-created`
does **not** print `outcome: ok`. Nothing is faked; the failure is a structured
`ClassNotFoundException`, not a fabricated success.

- Owner: P4a (continuation of the P4a agent closed by an infrastructure error).
- Time-box: 60 min hard checkpoint; stopped there.
- Runs under `tools/guarded.sh` (`--mem 6G`), no sudo, no commit.
- Host RAM during the runs: **26 / 60 GB used** (43 %, guard cap 4–6 GB/session).
- Files owned/changed: `crates/acos-art/src/launch.rs`,
  `crates/acos-art/guest/java/dev/acos/art/{LaunchMain,Nat}.java`,
  `crates/acos-binder-peer/{src/peer.rs,native/binder_driver.c}`,
  wiring in `crates/acos-art/{Cargo.toml,src/lib.rs,src/main.rs}`, this file.
  Two small cross-workstream edits are listed in §7.
- Not committed (as instructed).

```bash
cargo run -q -p acos-art -- launch --apk fixtures/build/apps/counter.apk \
    --until application-created
#   outcome=crash code=NO_APPLICATION  (blocker §4)
#   bind_application: request_bytes=1492 delivered=true app_thread_wire=1 code=5
#   21 service transactions listed (descriptor+code+method), unsupported=2
```

## 1. Acceptance

| requirement | result |
|---|---|
| `Looper.prepareMainLooper()`, `new ActivityThread()`, `thread.attach(false, startSeq)` | **PASS** — real `attach` via reflection, real
`IActivityManager.attachApplication(mAppThread, startSeq)` |
| peer/`ServiceHost` answers attach and sends `bindApplication` over the reverse delivery to the real `ApplicationThread` | **PASS** — `delivered=true`, `app_thread_wire=1`, `reverse_replies=1 reverse_failed=0`, `handleBindApplication` runs on the main looper |
| framework creates `LoadedApk`/`Application` from `fixtures/build/apps/counter.apk` | **PASS up to `LoadedApk`** — real `nativeloader` namespace + `DexPathList[[zip file "/data/app/counter.apk"]]`, real `ContextImpl`, real `getApplicationInfo` from the APK |
| `Application.onCreate` runs, proved by a marker | **FAIL** — blocked before `initInstrumentation` completes (§4) |
| stop before launching the Activity | **PASS** — no `ClientTransaction`, no `Activity.onCreate` |
| unknown transactions → structured `UNSUPPORTED_ANDROID_API` (JSONL), never faked | **PASS** — 2 records this run (§5) |
| list each service call during attach (descriptor+code) as the minimal surface | **PASS** — §6 |
| `cargo test -p acos-binder-peer` / `-p acos-art` | **PASS** — 3 + 9 tests, 24 tests |
| 3× deterministic `outcome: ok` | **not reached** (blocked on §4) |
| RAM < 90 %, `tools/guarded.sh`, no sudo | **PASS** — 26/60 GB, every run guarded, no privileged op |

## 2. The chain, with the artefact that proves each link

1. `LaunchMain` (guest, `guest/java/dev/acos/art/LaunchMain.java`):
   `Looper.prepareMainLooper()`, `new ActivityThread()` (private ctor via
   reflection), real `attach(false, seq)`; `Nat.startBinderLooper()` exports the
   same `IPCThreadState::joinThreadPool` entry the P2f rung uses, so
   `IApplicationThread` transactions are dispatched off the main thread, as in a
   real process. Marker: `ACOSLAUNCH MARK attach-returned startSeq=…`.
2. The peer (real `ServiceHost`) answers `IActivityManager.attachApplication`
   (code 25) from the host, records the `IApplicationThread` wire handle from
   the transaction arguments, and queues one `F_TXN` code 5 with the marshalled
   `bindApplication` request to that handle
   (`acos-binder-peer: delivering bindApplication (1492 bytes, code 5) to java binder wire=1`).
3. The request bytes are **written by the framework**: `LaunchMain` marshals
   `IApplicationThread.bindApplication` with the framework's own
   `writeToParcel`/`writeTypedObject` calls (ApplicationInfo, ProviderInfoList,
   ComponentName, ProfilerInfo, Bundle, Configuration, CompatibilityInfo,
   coreSettings, …) and pushes them to the peer-owned `acos.launch` node. No
   hand-written codec for this transaction.
4. `handleBindApplication` runs stock code: `ResourcesManager.applyConfiguration…`,
   `getInstrumentationInfoAsUser`, `notifyDexLoad`, `createClassloaderNamespace`
   (real `libnativeloader`), `getApplicationInfo`, `setupGraphicsSupport`,
   `ContextImpl.createAppContext`, … — every one of those is a real framework
   call, answered by the host (see §6).
5. Stop point: `ActivityThread.initInstrumentation` (§4). The activity is never
   launched.

## 3. What had to be added for the framework to get this far

| hole | fix |
|---|---|
| `android.hardware.OverlayProperties` natives | `register_android_hardware_OverlayProperties` (DisplayManagerGlobal's ctor touches the CREATOR even for a null reply) |
| `ActivityThread.nInitZygoteChildHeapProfiling` | `register_android_app_ActivityThread` |
| `ClassLoaderFactory.createClassloaderNamespace` | `register_com_android_internal_os_ClassLoaderFactory` (libnativeloader; the real APK class loader now builds) |
| guest code imported `dev.acos.e3g.Nat` from another crate's test dex | moved to `guest/java/dev/acos/art/Nat.java`; the driver exports `Java_dev_acos_art_Nat_*` **aliases** so the P2f acceptance dex (`dev.acos.e3g.Nat`) is unchanged and there is still one implementation |
| `android.os.Process.setArgV0` null `gCurRuntime` | unchanged from P4a: `LaunchMain.initAndroidRuntime` constructs the real `AndroidRuntime` from libandroid_runtime |
| package manager knows only the hello-views fixture | the launch peer now calls `ServiceHost::install_package_from_apk(counter.apk)`; `getApplicationInfo` returns the real APK's package |
| `IGameManagerService.notifyGraphicsEnvironmentSetup` (one-way) | `misc.rs` records it and returns nothing (no game-mode policy in ACOS to update) |
| `DisplayInfo` / `InstrumentationInfo` / `ApplicationInfo` reply codecs | see §7 (guest-marshalled reply templates, values are the display/app the launch serves) |

## 4. The blocker (exact)

```
java.lang.RuntimeException: Unable to instantiate instrumentation
  ComponentInfo{dev.acos.fixtures.counter/dev.acos.art.LaunchMain$Instr}:
  java.lang.ClassNotFoundException: Didn't find class
  "dev.acos.art.LaunchMain$Instr" on path:
  DexPathList[[zip file "/data/app/counter.apk"],nativeLibraryDirectories=[/data/app]]
    at android.app.ActivityThread.initInstrumentation(ActivityThread.java:7216)
    at android.app.ActivityThread.handleBindApplication(ActivityThread.java:6926)
```

`handleBindApplication` needs `mInstrumentation` to call
`Instrumentation.callApplicationOnCreate(app)` (`ActivityThread.java:7398` in
`android-14.0.0_r75`). With `instrumentationName != null` it loads the class
through **the app's class loader** (`data.info.getClassLoader()`), i.e. only
`/data/app/counter.apk` + its splits.

We tried the standard mechanism — declaring the launch instrumentation from a
**split APK**: the guest zips its own dex to `/data/app/launch-split.apk`,
sets `ApplicationInfo.splitSourceDirs/splitNames` on the bind request, and the
peer also replays a framework-marshalled `getApplicationInfo` reply that carries
the same split. The class loader is still built from the base APK only. Root
cause (from `LoadedApk.makePaths`, AOSP android-14):

```java
outZipPaths.add(aInfo.sourceDir);
if (aInfo.splitSourceDirs != null && !aInfo.requestsIsolatedSplitLoading())
    Collections.addAll(outZipPaths, aInfo.splitSourceDirs);
```

so `aInfo.splitSourceDirs` is null at that point. The `LoadedApk` is cached by
`ActivityThread.getPackageInfo` before `handleBindApplication`
(`GraphicsEnvironment.setup → PackageManager.getApplicationInfo`), and the
second `getPackageInfo(data.appInfo, …)` reuses that cached `LoadedApk`; the
split list from the bind request never reaches `makePaths`. (Not yet verified by
instrumenting `getPackageInfo` — that is the 10-minute step below.)

### Smallest next step (each ≤30 min, in order of preference)

1. **Give the fixture a real test instrumentation.** `fixtures/apps/counter`
   gets a second dex (`classes2.dex` or a test split APK) containing
   `dev.acos.fixtures.counter.test.LaunchInstr` and the launch names it. This is
   the path every real `am instrument` run takes; no framework seam is involved
   and the launch class is legitimately in the app's class loader.
2. **Make the `LoadedApk` cache honour the launch split.** Confirm with a print
   in `getPackageInfo`; then either push the framework-marshalled
   `getApplicationInfo` reply *before* `GraphicsEnvironment.setup` runs (it
   already is pushed before `attach`, so instrument the cache key) or use
   `ApplicationInfo.requestsIsolatedSplitLoading()`-style fields consistently.
3. **Drop the instrumentation and prove `onCreate` differently.** With
   `instrumentationName == null` the framework itself does
   `mInstrumentation = new Instrumentation(); mInstrumentation.callApplicationOnCreate(app)`
   (`ActivityThread.java:7326`, `7398`), so `Application.onCreate` **does** run
   without any class in the APK; the marker then has to come from something the
   app's `android.app.Application` does, or from `ActivityThread`'s
   `mInitialApplication` observed before `Looper.loop()` returns. Weakest proof
   of the three, but zero fixtures change.

## 5. Structured unsupported records (never faked)

`unsupported.jsonl` / `"unsupported"` this run, both `SERVICE_BAD_REQUEST` (not
fabricated successes):

| descriptor | code | method | cause |
|---|---|---|---|
| `android.content.pm.IPackageManager` | 106 | `notifyDexLoad` | AIDL arg table mismatch: `bad request … arg 3 (Str16): parcel underflow: need 13107296 byte(s) at 156, have 76`; API 34's `notifyDexLoad` takes a `Map<String,String>` class-loader context, so the generated tablet entry is stale |
| `android.app.IActivityManager` | 101 | `handleApplicationWtf` | arg-table mismatch on the framework's WTF report (it is reporting the instrumentation failure paths) |

The peer's `record_error` writes `"descriptor":"<unknown>"` because it re-peeks
the already-consumed request parcel — the correct descriptor is in the adjacent
`service_calls` entry. Worth fixing when the JSONL is consumed by tooling.

## 6. Minimal service surface seen during attach (this run, in order)

| # | descriptor | code | method |
|---|---|---|---|
| 1 | `android.app.IActivityManager` | 25 | `attachApplication` |
| 2 | `android.hardware.display.IDisplayManager` | 41 | `getPreferredWideGamutColorSpaceId` |
| 3 | `android.hardware.display.IDisplayManager` | 55 | `getOverlaySupport` |
| 4 | `android.hardware.display.IDisplayManager` | 1 | `getDisplayInfo` |
| 5 | `android.os.INetworkManagementService` | 42 | `setUidCleartextNetworkPolicy` |
| 6 | `android.hardware.display.IDisplayManager` | 1 | `getDisplayInfo` |
| 7 | `android.content.pm.IPackageManager` | 44 | `getInstrumentationInfoAsUser` |
| 8 | `android.content.pm.IPackageManager` | 106 | `notifyDexLoad` *(unsupported)* |
| 9 | `android.hardware.display.IDisplayManager` | 1 | `getDisplayInfo` |
| 10 | `android.app.IActivityManager` | 9 | `checkPermission` |
| 11 | `android.net.IConnectivityManager` | 32 | `getProxyForNetwork` |
| 12 | `android.content.pm.IPackageManager` | 20 | `getPackagesForUid` |
| 13 | `android.content.pm.IPackageManager` | 9 | `getApplicationInfo` |
| 14 | `android.app.IGameManagerService` | 4 | `isAngleEnabled` |
| 15–18 | `android.content.pm.IPackageManager` | 100 | `hasSystemFeature` ×4 |
| 19 | `android.app.IActivityManager` | 101 | `handleApplicationWtf` *(unsupported)* |
| 20 | `android.content.pm.IPackageManager` | 9 | `getApplicationInfo` |
| 21 | `android.app.IActivityManager` | 9 | `checkPermission` |

So the minimal AMS/PMS/DMS surface for a framework boot is: **attachApplication,
checkPermission, handleApplicationWtf; getApplicationInfo, getInstrumentationInfoAsUser,
notifyDexLoad, getPackagesForUid, hasSystemFeature; getDisplayInfo,
getPreferredWideGamutColorSpaceId, getOverlaySupport; getProxyForNetwork;
setUidCleartextNetworkPolicy; isAngleEnabled + notifyGraphicsEnvironmentSetup**
(the last two arrive after `setupGraphicsSupport`).

## 7. Cross-workstream seams (explicit, each one a `TODO` for its owner)

1. **`acos-services` `DisplayInfo` codec is pre-API-34.** Proven with the repo's
   own DEX scanner
   (`cargo run -p acos-services --example scan-parcelables -- .cache/boot/android-34/dex/framework android.view.DisplayInfo`):
   the real `writeToParcel` is
   `5×writeInt, 2×writeParcelable, writeString8, 8×writeInt,
   DisplayCutout$ParcelableWrapper.writeCutoutToParcel, 2×writeInt, writeFloat,
   2×writeInt, Display$Mode.writeToParcel, 2×writeInt, writeInt, writeParcelable,
   writeBoolean, writeInt, 2×writeFloat, 2×writeLong, 3×writeInt, 2×writeString8,
   writeInt, 4×writeFloat, writeTypedObject, 3×writeInt, 2×writeTypedObject,
   writeFloat, writeSparseArray, writeString8`
   (224-byte output for a default `DisplayInfo`, cross-checked byte-for-byte
   against a framework-marshalled probe on ART). The current 21-field spec
   describes an older layout and produces a parcel that fails in the framework's
   own reader (`DisplayCutout.readCutoutFromParcel → bad array lengths`).
2. **Guest-marshalled reply templates in the peer** (`acos.launch` code 2,
   `Peer::launch_reply_for`): for `getDisplayInfo`, `getInstrumentationInfoAsUser`
   and `getApplicationInfo` the guest marshals the reply with the framework's own
   `writeToParcel` and the peer replays it verbatim. The *transactions* are real
   host transactions and are still listed in the service surface; only the
   parcel writer is the framework's. This is a **temporary seam**: when (1) is
   fixed, delete the display template; the instrumentation/application templates
   go away with §4's fix.
3. **`acos-services::misc.rs`**: one match arm,
   `"notifyGraphicsEnvironmentSetup" => Vec::new()` (one-way notification, no
   game-mode policy to update).
4. **`acos-art` now depends on `acos-services`** (path dep) only for
   `ServiceHost::standard()` + `install_package_from_apk` in `launch.rs`.
5. **`launch-guest` sources**: `launch.rs` compiles only
   `LaunchMain.java`, `RenderNat.java`, `Nat.java` from `guest/java`, so unrelated
   guest code cannot break the command.

## 8. Reproduce / diagnose

```bash
# acceptance command (currently: outcome=crash NO_APPLICATION, §4)
tools/guarded.sh --mem 6G --time 300 -- cargo run -q --release -p acos-art -- \
    launch --apk fixtures/build/apps/counter.apk --until application-created

# every transaction + template + reverse-delivery line
E3G_DEBUG=1 … # (peer lines are on stderr: "acos-binder-peer: …")

# framework-marshalled DisplayInfo probe (bytes + framework read-back)
ACOSLAUNCH_PROBE=1 … # prints "ACOSLAUNCH PROBE displayinfo-write len=224 hex=…"

cargo test -q -p acos-binder-peer   # 3 protocol + 9 peer tests: ok
cargo test -q -p acos-art          # 24 tests: ok
```

Guest verdict markers emitted before the blocker (all from real framework calls):
`android-runtime ready`, `binder-looper started`, `bind-request-marshalled`,
`attach-returned`, `displayinfo-reply-pushed`, `applicationinfo-reply-pushed`,
`instrumentationinfo-reply-pushed`.
