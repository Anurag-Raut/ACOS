# MODTAIL addendum — the attached case per gap name (re-check of AOSP_DECISIONS.md)

`results/handoff/MOD0/AOSP_DECISIONS.md` decides what a gap name answers while no
module owns it (**tombstone** = structured `UNSUPPORTED_ANDROID_API`, **null** =
AOSP's own missing path). This file records what changed for the *attached* case,
and the two decisions that turned out to be incomplete.

## Names this track now owns

| name | module | while off (unchanged) | while attached | notes |
|---|---|---|---|---|
| `phone` | `net.telephony` | null | `ITelephony` from the image: sim state, phone type, network type, ICC, line-1, call state | **MOD1's null decision stands for `off`** (its client source null-checks the proxy), but the attached case needs more than a binder: `TelephonyManager.getITelephony()` dereferences `TelephonyFrameworkInitializer.getTelephonyServiceManager()`, which on a device the telephony module's **process** installs. Null there is not a "handled missing path" — it is an NPE inside `getSimState()`/`listen()`/`SmsManager`. The session therefore installs `TelephonyServiceManager` in the guest before the app boots when the module is attached (`ACOSLAUNCH_TELEPHONY`). |
| `isms`, `iphonesubinfo`, `telephony.registry` | `net.telephony` | null (not in the tombstone table; no client reads them directly) | the same module | `SmsManager` reaches `isms` lazily; a phone-model `ISms` answers `sendTextForSubscriber`, `isSmsSimPickActivityNeeded`, `isImsSmsSupportedForSubscriber`, `getPreferredSmsSubscription`, `getImsSmsFormatForSubscriber`, `isSMSPromptEnabled`. |
| `carrier_config` | `net.telephony` | null (feature-gated factory) | still `off` | `CarrierConfigManager` is registered only for `FEATURE_TELEPHONY_SUBSCRIPTION`; the module reports the gap, and the report string says `carrier_config` is not served. |
| `auth` | `hw.biometric` | tombstone | `IAuthService` (`canAuthenticate`, `authenticate`, `cancelAuthentication`, `hasEnrolledBiometrics`, `getAuthenticatorIds`) | With the module off, the tombstone makes `BiometricManager.canAuthenticate` throw `UnsupportedOperationException` (the client does not catch it: it is not a `RemoteException`). That is the decision's documented cost; the detached scenario records it as `BIOMETRIC unavailable=UnsupportedOperationException`. |
| `fingerprint` | `hw.biometric` | tombstone | still tombstone | no fingerprint HAL; the module never provides this name. |
| `bluetooth`, `bluetooth_manager` | `hw.bluetooth` | null | `IBluetoothManager`/`IBluetooth` | The `null` decision is right for `off` (`BluetoothAdapter.createAdapter` builds a null adapter). For the attached case, `BluetoothAdapter.createAdapter` dereferences `BluetoothFrameworkInitializer.getBluetoothServiceManager()` — installed by the Bluetooth mainline module on a device — so the session installs it too (`ACOSLAUNCH_BLUETOOTH`). **Blocked:** with both installed the guest still hangs in `ServiceManager.waitForService("bluetooth_manager")` (see MODTAIL/README.md). |
| `usb` | `hw.usb` | tombstone | stub: `getDeviceList` → empty `Bundle`, `getCurrentFunctions` → `0` | the honest "no USB device" answer; the tombstone's throwing binder is what a `getServiceOrThrow` client already saw. |
| `dropbox` | `system.dropbox` | tombstone | stub: `isTagEnabled` → false, `getNextEntry[WithAttribution]` → null | **fixed:** the tombstone's AIDL descriptor was `android.os.IDropBoxManagerService`; the image's client writes `com.android.internal.os.IDropBoxManagerService`, so the old tombstone could not even match the interface token. Corrected in `TOMBSTONES`. |
| `usagestats` | `system.usage` | tombstone | off only | the queries return `ParceledListSlice<UsageStats>`; an empty slice would be indistinguishable from "not used". |
| `device_policy` | `system.dpm` | tombstone | off only | no managed device/profile to describe. |
| `search` | `system.search` | tombstone | off only | no searchable index; empty `SearchableInfo` lists would look like a provider. |
| `role` | `system.role` | tombstone | off only | `IRoleManager` lives in the Permission APEX; its transaction table is not in the framework dex this track reads, so a stub would guess codes. Needs `packages/modules/Permission`'s dex in the reference cache. |
| `wallpaper` | `system.wallpaper` | null | off only | AOSP's null path is `DisabledWallpaperManager`, a working manager. |
| `print` | `system.print` | tombstone | off only | the client only looks the name up under `FEATURE_PRINTING`, which ACOS reports absent. |
| `nfc` | `hw.nfc` | null | off only | `NfcAdapter` throws before any `ServiceManager` lookup. |
| `country_detector` | `geo.country` | tombstone | off only | a detected country is a location claim ACOS cannot back. |

## New rule learned by this track

A module's attach is not the only precondition for an AOSP client to work: several
mainline clients dereference a **process-installed seam** before any binder call
(`TelephonyFrameworkInitializer.getTelephonyServiceManager()`,
`BluetoothFrameworkInitializer.getBluetoothServiceManager()`,
`WifiFrameworkInitializer`'s manager, `MediaFrameworkInitializer`'s). On a device
the mainline module's own process installs it. A module that owns such a client
must be `at-launch` and the session must install the seam in the guest, or the
list of transaction codes it answers is unreachable. That is now the rule for the
long tail: check the client's `*FrameworkInitializer.get…Manager()` before
promising a name.
