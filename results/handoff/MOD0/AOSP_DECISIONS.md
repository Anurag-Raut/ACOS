# AOSP decision per tombstone name (ADR-0029 item 3)

Generated from `crates/acos-services/src/module.rs::TOMBSTONES`, the same table
`acos_capabilities.modules.gaps` and the generated `acos_help` limits table render.

Rule (from the brief):

* **Tombstone** - the name answers a binder whose every transaction is a structured
  `UNSUPPORTED_ANDROID_API` naming the module and the attach call. Chosen where AOSP
  only *stores* the proxy during initialization (a throwing binder cannot break
  framework init) and where today's `null` either throws at `getSystemService`
  (`getServiceOrThrow`) or makes the call a silent no-op / silent NPE.
* **Null** - the name keeps Android's missing path (`checkService` answers `null`) and
  is reported `off` with the reason. Chosen where AOSP builds *working* behaviour out
  of `null` (a disabled manager, a feature-gated factory, a `try/catch` that reports
  the failure), or where the name is reached from native code and a Java tombstone
  would be bypassed.

The ten rows that were previously `null` only because their client class was not in
the offline cache are re-decided **from source** (MOD1, 2026-11-16): the cache now
carries `packages/modules/Wifi`, `packages/modules/Bluetooth`,
`packages/modules/Permission` and the `base` client paths
(`telephony/`, `media/`, `nfc/`, `core/java/android/app/role`), see
`tools/fetch-aosp-reference.sh` and `docs/AOSP_REFERENCE.md`. Four of them became
tombstones (the client stores the proxy and only fails at first use); six stay
`null` because AOSP's own null path is the working, honest one.

| # | ServiceManager name | module | decision | AIDL descriptor | reason | AOSP evidence |
|---|---|---|---|---|---|---|
| 1 | `vibrator_manager` | `feedback.vibrator` | **tombstone** | `android.os.IVibratorManagerService` | no vibrator hardware model; `Vibrator.vibrate()` is a silent no-op today | base/core/java/android/os/SystemVibratorManager.java:57-63 stores the proxy in its constructor (no transaction at init); SystemVibrator.java:70-78 logs `Failed to retrieve vibrator ids; no vibrator manager service` and returns a silent no-op for a null manager |
| 2 | `vibrator` | `feedback.vibrator` | **null** | `android.os.IVibrator` | no vibrator hardware model | SystemVibrator does not look `vibrator` up itself; it resolves `vibrator_manager` (SystemVibrator.java:61) and does everything through it, so the tombstone there is the one that matters and this name stays null as on API-34 (no client reads it) |
| 3 | `usb` | `hw.usb` | **tombstone** | `android.hardware.usb.IUsbManager` | no USB host/device model | SystemServiceRegistry.java:735-740 fetches with getServiceOrThrow, so a null name already throws ServiceNotFoundException out of getSystemService(UsbManager.class); a tombstone moves that to a structured error at first use and cannot break init |
| 4 | `midi` | `hw.midi` | **tombstone** | `android.media.midi.IMidiManager` | no MIDI device model | SystemServiceRegistry.java:1077-1083 getServiceOrThrow + `new MidiManager(proxy)`; null already throws at getSystemService |
| 5 | `launcherapps` | `system.launcher` | **tombstone** | `android.content.pm.ILauncherApps` | no launcher and no launcher-apps service | LauncherApps.java:672-675 stores the proxy (`this(context, Stub.asInterface(getService(...)))`) and never transacts in the constructor; a null binder is a silent NPE later |
| 6 | `usagestats` | `system.usage` | **tombstone** | `android.app.usage.IUsageStatsManager` | no usage-stats service (nothing records app usage) | SystemServiceRegistry.java:1022-1028 getServiceOrThrow + UsageStatsManager.java:448-451 stores the proxy; null already throws at getSystemService |
| 7 | `dropbox` | `system.dropbox` | **tombstone** | `android.os.IDropBoxManagerService` | no DropBox service | SystemServiceRegistry.java:488-494 getServiceOrThrow + DropBoxManager.java:299-302 stores the proxy; null already throws at getSystemService |
| 8 | `device_policy` | `system.dpm` | **tombstone** | `android.app.admin.IDevicePolicyManager` | no device-policy manager (no managed device/profile) | SystemServiceRegistry.java:462-466 getServiceOrThrow + DevicePolicyManager.java:246-252 stores the proxy; null already throws at getSystemService |
| 9 | `batterystats` | `system.battery` | **tombstone** | `com.android.internal.app.IBatteryStats` | no battery-stats service | SystemServiceRegistry.java:1426-1436 getServiceOrThrow; null already throws at getSystemService |
| 10 | `fingerprint` | `hw.biometric` | **tombstone** | `android.hardware.fingerprint.IFingerprintService` | no fingerprint/biometric HAL | SystemServiceRegistry.java:916-927 getServiceOrThrow for targetSdk >= O; null already throws at getSystemService |
| 11 | `auth` | `hw.biometric` | **tombstone** | `android.hardware.biometrics.IAuthService` | no biometric authentication service | SystemServiceRegistry.java:958-967 getServiceOrThrow(Context.AUTH_SERVICE) for BiometricManager; null already throws at getSystemService |
| 12 | `print` | `system.print` | **tombstone** | `android.print.IPrintManager` | no print service | SystemServiceRegistry.java:857-868 only looks the name up when PMS reports FEATURE_PRINTING; ACOS reports the feature absent, so a tombstone is only reached if the feature is turned on and then answers the structured error instead of a null proxy |
| 13 | `country_detector` | `geo.country` | **tombstone** | `android.location.ICountryDetector` | no country detector | SystemServiceRegistry.java:454-459 getServiceOrThrow; null already throws at getSystemService |
| 14 | `wallpaper` | `system.wallpaper` | **null** | `android.app.IWallpaperManager` | no wallpaper service | SystemServiceRegistry.java:774-793 turns a null binder into `DisabledWallpaperManager` (config_enableWallpaperService false) — a working, honest \ |
| 15 | `wifi` | `net.wifi` | **null** | no Wi-Fi stack (the network module owns real internet, ADR-0030) | packages/modules/Wifi framework/java/android/net/wifi/WifiFrameworkInitializer.java:75-85 registers a context-aware factory that returns `null` when PMS reports no FEATURE_WIFI; WifiManager.java:2202-2209 only stores the proxy/looper. ACOS reports no Wi-Fi feature, so Android's real missing path already answers `null`. |
| 16 | `phone` | `net.telephony` | **null** | no telephony stack | base/telephony/java/android/telephony/TelephonyManager.java:513-524 constructs the manager from the context only (no binder at init); the service is resolved lazily in getITelephony() (:6641-6649) and callers handle a null proxy (:579-588 return null, :480-488 fall back to INVALID_SUBSCRIPTION_ID). A tombstone would turn handled calls into binder exceptions. |
| 17 | `bluetooth` | `hw.bluetooth` | **null** | no Bluetooth stack | packages/modules/Bluetooth framework/java/android/bluetooth/BluetoothAdapter.java:1096-1114 logs `BluetoothServiceManager is null` / `Bluetooth service is null` and returns a null adapter (BluetoothManager.java:65-67 passes that through) - AOSP's documented no-Bluetooth path. A tombstone would make the adapter constructor throw while initialising the framework's own service. |
| 18 | `bluetooth_manager` | `hw.bluetooth` | **null** | no Bluetooth stack | as `bluetooth`: BluetoothFrameworkInitializer.java:99-104 always builds BluetoothManager, but BluetoothManager.java:65-67 returns the null adapter from BluetoothAdapter.createAdapter (BluetoothAdapter.java:1096-1114), so no binder is ever required for `bluetooth_manager`. |
| 19 | `nfc` | `hw.nfc` | **null** | no NFC stack | base/nfc/java/android/nfc/NfcAdapter.java:749-780 gates on FEATURE_NFC/FEATURE_NFC_HOST_CARD_EMULATION and throws UnsupportedOperationException when the device has none, before any ServiceManager lookup; ACOS reports the features absent, so `null` (the name is never read) is AOSP's real missing path. |
| 20 | `media.camera` | `hw.camera` | **null** | `android.hardware.ICameraService` | no camera HAL | CameraManager reaches the camera service from native code (CameraManagerGlobal), not through the Java ServiceManager path; the module must provide the HAL seam, so a Java tombstone would be bypassed |
| 21 | `media_session` | `media.session` | **tombstone** | no media-session service | base/media/java/android/media/session/MediaSessionManager.java:124-133 stores the proxy from MediaFrameworkPlatformInitializer.getMediaSessionServiceRegisterer().get() without transacting (plus MEDIA_COMMUNICATION_SERVICE), so a throwing binder cannot break getSystemService; null makes every later call an NPE. |
| 22 | `media_router` | `media.session` | **tombstone** | no media-router service | base/media/java/android/media/MediaRouter.java:140-150 builds the proxy from ServiceManager.getService(MEDIA_ROUTER_SERVICE) and Static.startMonitoringRoutes -> rebindAsUser (:443-456) calls registerClientAsUser inside `try/catch (RemoteException)`, so a tombstone is caught and logged while a null binder is an NPE thrown out of getSystemService(MEDIA_ROUTER_SERVICE). |
| 23 | `search` | `system.search` | **tombstone** | no search service | base/core/java/android/app/SearchManager.java:557-562 builds the proxy with `ISearchManager.Stub.asInterface(ServiceManager.getServiceOrThrow(SEARCH_SERVICE))` in its constructor, so a null name already throws ServiceNotFoundException out of getSystemService(SearchManager.class); a tombstone moves that to a structured error at first use and cannot break init (the constructor never transacts). |
| 24 | `role` | `system.role` | **tombstone** | no role manager | packages/modules/Permission framework-s/java/android/app/role/RoleFrameworkInitializer.java:44-46 registers `new RoleManager(context, IRoleManager.Stub.asInterface(serviceBinder))` with no null check (a null binder builds a RoleManager that NPEs on every call); RoleManager.java:282-285 only stores the proxy, so a tombstone answers a structured error at first use. |
| 25 | `carrier_config` | `net.telephony` | **null** | no telephony stack | TelephonyFrameworkInitializer.java:83-86 registers CarrierConfigManager only when PMS reports FEATURE_TELEPHONY_SUBSCRIPTION, and CarrierConfigManager.java:151-153 stores the context without resolving the binder; ACOS reports no telephony feature, so Android's real missing path (`null`) is already the honest answer and a tombstone would never be reached. |
| 26 | `webviewupdate` | `web.webview` | **null** | no WebView provider yet (ADR-0031) | base/core/java/android/webkit/WebViewFactory.java:614-625 resolves the service lazily and :295-300 catches the failure of `waitForAndGetProvider()` and returns a LIBLOAD_* status, so null is a handled, reported path; a tombstone would only change the logged exception, not the outcome. |

## Summary

* 26 gap names from `docs/STATE_AND_PLAN_2026-11-15.md` §6: **16 tombstones**, 10 kept `null`.
* Every one appears in `acos_capabilities.modules.gaps` with state, reason and this
  evidence string, and in the generated limits table of `acos_help`.
* Every row is now decided from source (MOD1 widened the offline cache; see the rule
  above). The rows that stay `null` are the ones where AOSP's own missing path works:
  a feature-gated factory (wifi, carrier_config), a documented null adapter
  (bluetooth, bluetooth_manager), a feature check that throws before the lookup
  (nfc), lazily null-checked calls (phone), a caught-and-reported failure
  (webviewupdate), a disabled manager (wallpaper) or a native-code path (media.camera).
