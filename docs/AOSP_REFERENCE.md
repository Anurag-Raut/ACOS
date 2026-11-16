# AOSP source reference for agents (read-only)

Local sparse checkouts of Google's open-source Android at tag android-14.0.0_r75 (approx. 73 MB), in `/home/Qui/Projects/acos/.cache/aosp/` (gitignored; recreate with `tools/fetch-aosp-reference.sh`).
Use them to understand behaviour and wire formats BEFORE guessing or disassembling. Do not copy source into the repo; the compiled binaries from the API-34 system image stay the thing we run. The tag is close to, not identical to, the image build: verify against the image when it matters (ELF build ID, disassembly).

| Dir | Contents | Use for |
|---|---|---|
| `native/` | libs/binder (IPCThreadState, ProcessState, Parcel, BpBinder, ndk), include/binder, libs/ui, libs/gui, **libs/sensor** (ISensorServer, ISensorEventConnection, Sensor, SensorEventQueue, SensorManager), **services/sensorservice** | Binder driver protocol (BC_*/BR_*), node refcounting, flat_binder_object, stability; the native sensor service's wire (`Sensor::flatten`, `BitTube` event packets, `enableDisable`/`setEventRate`/`flush`) |
| `base/` | core/jni (AndroidRuntime.cpp, android_os_*, registration order), libs/hwui (+ jni/), libs/androidfw, core/java/android/os, com/android/internal/os | libandroid_runtime registration, graphics JNI, AssetManager/overlays, zygote/RuntimeInit |
| `base/services/core/java/com/android/server/policy/` | WindowManagerPolicy.java, PhoneWindowManager.java and the rest of the policy package | the **window layer table**: `getWindowLayerFromTypeLw()` (WindowManagerPolicy.java:525-630) and `getSubWindowLayerFromTypeLw()` (:650-671), transcribed into `crates/acos-wm/src/layers.rs` |
| `base/services/core/java/com/android/server/wm/` | WindowManagerService, DisplayArea, WindowState, … | how the WMS consumes the policy layers (`DisplayArea` above/below `APPLICATION_LAYER`), `canAddInternalSystemWindow` |
| `art/` | runtime, libartbase, libnativeloader, libnativebridge | boot image/classpath logic (e.g. mainline CHECK), linker namespaces, runtime flags |
| `bionic/` | linker, libc/bionic, libc/include | linker namespaces, property area, sysconf |
| `libbase/`, `core/` | libbase, libutils, libcutils, liblog, property_service | RefBase, logging, properties |
| `connectivity/`, `wifi/`, `bluetooth/`, `permission/` | `framework/src/android/net`, `framework/java/android/net/wifi`, `framework/java/android/bluetooth`, `framework-s/java/android/app/role` | the mainline service-client classes behind the ADR-0029 tombstone table (feature gates, lazy proxies) |

`base/` also carries the client paths the tombstone table cites:
`core/java/android/{app,content,net,os,view,webkit}`, `media/java/android/media`
(+ `session/`), `nfc/java/android/nfc`, `telephony/java/android/telephony`,
`hardware/java/android/hardware` (which carries `Sensor.java`,
`SensorEvent.java`, `SensorManager.java` and `SystemSensorManager.java` — the
client half of the native sensor stack the `sensors` module serves).

Sparse checkouts: add more paths with `git -C <dir> sparse-checkout add <path>` (blobs are fetched on demand, partial clone). Cone mode only accepts directories; a single file path needs `--skip-checks` (which is what `tools/fetch-aosp-reference.sh` uses for `MediaRouter.java`).

The window-layer table is also generated from this cache into `docs/WINDOW_LAYERS.md`
with `tools/gen-window-layer-table.py` (cite file:line per row; re-run it after a
tag change and diff the output against the transcribed table).
- `ActivityManager.INTENT_SENDER_*` (the `IIntentSender`/`getIntentSender` type) are
  **BROADCAST=1, ACTIVITY=2, ACTIVITY_RESULT=3, SERVICE=4, FOREGROUND_SERVICE=5**
  (`base/core/java/android/app/ActivityManager.java:603-632`). The image's
  `PendingIntent.getBroadcastAsUser` really sends `1` (PIFILLIN dex probe), so a
  sender's `type` must not be read as "1 = activity": an earlier build had the
  four shifted by one and delivered every `getBroadcast` sender as an activity
  start. `Location.writeToParcel` in this image has no mock bit (bit 4 is
  vertical accuracy), and `ActivityManager$PendingIntentInfo.writeToParcel` is
  `(String, int, boolean, int)` — the reply `getInfoForIntentSender` must carry.

## Where the per-process mainline bootstrap runs (image authority, GATENBOOT)

`ActivityThread.initializeMainlineModules()` is called from **`ActivityThread.main(String[])`**, not
from `handleBindApplication`. Source and image agree:

* `base/core/java/android/app/ActivityThread.java:8558-8561` — `main()` calls it after
  `Environment.initForCurrentUser()` / `TrustedCertificateStore.setDefaultUserDirectory(...)` and
  **before** `Process.setArgV0("<pre-initialized>")` and `Looper.prepareMainLooper()`;
  the body is `:8601-8610`.
* Image dex: `framework/classes.dex`, `ActivityThread.main` at `1c37c0`, insn `001c`
  `invoke-static {}, Landroid/app/ActivityThread;.initializeMainlineModules:()V` (the only call site
  in `classes.dex`; `handleBindApplication` does not call it).
* The **image's** list is shorter than r75's: `TelephonyServiceManager`, `StatsServiceManager`,
  `MediaServiceManager` ×2 (`MediaFrameworkPlatformInitializer` **and** `MediaFrameworkInitializer`),
  `BluetoothServiceManager` + `setBinderCallsStatsInitializer`, `NfcServiceManager`,
  `DeviceConfigServiceManager`. r75 also has `SeFrameworkInitializer`,
  `ProfilingFrameworkInitializer` (behind `android.server.Flags.telemetryApisService()`) and a
  `ConnectivityFrameworkInitializer` call exists in other builds — none of the three is in this
  image's body, so the image is what a guest may rely on. `android/server/Flags` is not in this
  image's dexes at all.
* Consequence for ACOS: any process that builds the `ActivityThread` itself (ACOS's guest
  launcher does, instead of running `ActivityThread.main`) must call the same framework method once,
  before app code. `ModuleManifest::process_initializers` is only for a manager that call does not
  install — telephony and Bluetooth are already covered, and both setters throw
  `IllegalStateException` on a second call.
