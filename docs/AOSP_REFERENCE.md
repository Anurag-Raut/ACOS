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
