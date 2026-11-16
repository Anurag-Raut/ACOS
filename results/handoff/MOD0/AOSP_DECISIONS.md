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
  of `null` (a disabled manager), where the client source is not in the offline AOSP
  cache (`.cache/aosp` is `framework/base` only, so a throwing binder is unverified),
  or where the name is reached from native code and a Java tombstone would be bypassed.

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
| 15 | `wifi` | `net.wifi` | **null** | `android.net.wifi.IWifiManager` | no Wi-Fi stack (the network module owns real internet, ADR-0030) | WifiManager's client source lives under packages/modules/Wifi, which is not in the offline AOSP cache (.cache/aosp is framework/base only), so a present-but-throwing binder is unverified; null is today's behaviour and cannot crash init |
| 16 | `phone` | `net.telephony` | **null** | `com.android.internal.telephony.ITelephony` | no telephony stack | TelephonyManager is not in the offline AOSP cache; null is today's behaviour |
| 17 | `bluetooth` | `hw.bluetooth` | **null** | `android.bluetooth.IBluetoothManager` | no Bluetooth stack | BluetoothAdapter/BluetoothManager are not in the offline AOSP cache (packages/modules/Bluetooth); null is today's behaviour |
| 18 | `bluetooth_manager` | `hw.bluetooth` | **null** | `android.bluetooth.IBluetoothManager` | no Bluetooth stack | as `bluetooth`: client source not cached |
| 19 | `nfc` | `hw.nfc` | **null** | `android.nfc.INfcAdapter` | no NFC stack | NfcAdapter is not in the offline AOSP cache (packages/apps/Nfc); null is today's behaviour |
| 20 | `media.camera` | `hw.camera` | **null** | `android.hardware.ICameraService` | no camera HAL | CameraManager reaches the camera service from native code (CameraManagerGlobal), not through the Java ServiceManager path; the module must provide the HAL seam, so a Java tombstone would be bypassed |
| 21 | `media_session` | `media.session` | **null** | `android.media.session.ISessionManager` | no media-session service | MediaSessionManager's client source is not in the offline AOSP cache; null is today's behaviour |
| 22 | `media_router` | `media.session` | **null** | `android.media.IMediaRouterService` | no media-router service | MediaRouter static initialization is not in the offline AOSP cache; a throwing binder there is unverified |
| 23 | `search` | `system.search` | **null** | `android.app.ISearchManager` | no search service | SearchManager's constructor fetches the service itself (app/SearchManager.java has no `public SearchManager(Context)`); its init path is unverified, so null stays |
| 24 | `role` | `system.role` | **null** | `android.app.role.IRoleManager` | no role manager | RoleManager is not in the offline AOSP cache; null is today's behaviour |
| 25 | `carrier_config` | `net.telephony` | **null** | `com.android.internal.telephony.ICarrierConfigLoader` | no telephony stack | CarrierConfigManager is not in the offline AOSP cache; null is today's behaviour |
| 26 | `webviewupdate` | `web.webview` | **null** | `android.webkit.IWebViewUpdateService` | no WebView provider yet (ADR-0031) | reachable only through WebViewFactory, which the package manager reports absent; null is today's behaviour |

## Summary

* 26 gap names from `docs/STATE_AND_PLAN_2026-11-15.md` §6: **12 tombstones**, 14 kept `null`.
* Every one appears in `acos_capabilities.modules.gaps` with state, reason and this
  evidence string, and in the generated limits table of `acos_help`.
* The `null` rows whose client source is outside `framework/base` (wifi, phone,
  bluetooth, nfc, media_session, media_router, search, role, carrier_config,
  webviewupdate) are conservative: the smallest next action is to add the client class
  to the AOSP cache and re-decide the row (the table is data).
