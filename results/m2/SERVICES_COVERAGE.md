# Service coverage — T-38 (IME) + T-39 (remaining services)

Wave W30-A, base `5c5162d`. Every row names the **served** transaction and what
the caller gets. Codes/layouts come from the decoded `acos-aidl` tables, which
are generated from the real boot-classpath `$Stub` (never hand-typed).

Status legend

| status | meaning |
|---|---|
| **ok** | decoded, state machine serves the reply; covered by a test |
| **oneway** | decoded, accepted and recorded (no reply by contract) |
| **state** | state machine + tests done; the reply is a real framework parcelable that needs the Java marshaller (`*_java` rewrite) — the transport refuses it with a **structured** error until then, never fake bytes |
| **unsupported** | decoded, but Android would change system state / ACOS cannot do it: `UNSUPPORTED_ANDROID_API` with interface+code |
| **absent** | no `$Stub` in the extracted boot classpath (documented gap) |

## Tables extended for this wave

`crates/acos-aidl/src/generate.rs` now also decodes the **declared** service set
(`DECLARED_INTERFACES`), because the measured hello-views trace only contains
the calls the reference apps make:

| interface | transactions decoded |
|---|---|
| `android.content.IClipboard` | 12 |
| `android.os.IUserManager` | 97 |
| `android.app.INotificationManager` | 148 |
| `com.android.internal.statusbar.IStatusBarService` | 73 |
| `android.permission.IPermissionManager` | 28 |
| `com.android.internal.app.IAppOpsService` | 50 |
| `com.android.internal.app.IBatteryStats` | 100+ |
| `android.hardware.ISensorPrivacyManager` | 1 (isToggleSensorPrivacyEnabled) |
| `android.net.IConnectivityManager` | 91 |

Regenerated with `cargo run -p acos-aidl --example gen` → 37 measured+declared
interfaces, 0 missing, 1 pre-existing `Unknown`
(`IApplicationThread.bindApplication` map key/value). `IBatteryStats` had to be
spelled `com.android.internal.app.IBatteryStats` and sensor privacy
`android.hardware.ISensorPrivacyManager` (the trace-file guesses did not exist).

`IBatteryStats`/`ISensorPrivacyManager` are decoded but have **no service
registered**: nothing in the trace or the fixture apps call them, so
`ServiceManager.checkService` returns `null` exactly as for any absent service
(not a fake stub). Wiring them is a state-machine-only follow-up.

## IInputMethodManager — the null in-process IME (`acos-services::ime`)

| transaction | status | reply / evidence |
|---|---|---|
| `addClient(IInputMethodClient, IRemoteInputConnection, int)` | ok | recorded; idempotent per client binder |
| `startInputOrWindowGainedFocus(...)` | ok | `InputBindResult` materialised by `acos-runtime::ime_java`: the framework's own `InputBindResult.NO_IME.result` (measured **6** in API 34; not `1`), no `IInputMethodSession`, no channel; recorded in `ImeState::start_inputs` |
| `showSoftInput` | ok | `false` + `ResultReceiver` answered (no hang) |
| `hideSoftInput` | ok | `false` |
| `isImeTraceEnabled` | ok | `false` |
| `reportPerceptibleAsync` | oneway | recorded |
| `removeImeSurfaceFromWindowAsync` | oneway | recorded (no surface) |
| `getImeTrackerService` | ok | `null` (no tracker binder to hand out before a bound IME) |
| `getInputMethodList`/`getEnabledInputMethodList` | ok | empty list |
| `getCurMethod`/`getLastInputMethod` | ok | `null` |
| `isInputMethodShown` | ok | `false` |
| `IInputMethodSession.viewClicked` | ok | counted |
| `IInputMethodSession.updateSelection` | ok | counted (measured 11× interaction) |
| `IInputMethodSession.appPrivateCommand/displayCompletions/updateCursor/updateExtractedText/toggleSoftInput/finishInput` | ok | accepted no-ops (IME→app direction) |
| `IImeTracker.onRequestShow/onShown/onRequestHide/onHidden/onProgress` | state | recorded; `progress_count` reaches the measured 22 ± 3 |
| `IImeTracker.hasPendingCancellation` | ok | `false` |
| anything else (`setInputMethod`, `setInputMethodAndSubtype`, ...) | unsupported | `UNSUPPORTED_ANDROID_API` |
| `IInputMethodClient.setActive/onBindMethod/reportFullscreenMode`, `IRemoteInputConnection.*` | owner = `acos-vm (delivery)` | these are app-side objects delivered by the VM, not services |

**Declared divergence (evidence, not a hang):** no soft keyboard, no IME bound,
`InputBindResult.NO_IME`. Typed text is injected as `KeyEvent`s through the
input dispatcher (`acos_services::input` → window `InputChannel`), so
`InputConnection.commitText` is never on the path. `ImeState::capabilities()`
emits this as JSON for the session evidence.

## IClipboard (`acos-services::clipboard`)

| transaction | status | reply / evidence |
|---|---|---|
| `setPrimaryClip(ClipData, ...)` / `setPrimaryClipAsPackage` | ok | `acos-runtime::clipboard_java` reads the real `ClipData` through its own `CREATOR`, stores the bytes its own `writeToParcel` emits + facts read from the real object, and delivers `onPrimaryClipChanged` oneway per registered listener |
| `getPrimaryClip` | ok | the stored bytes verbatim → `ClipData.CREATOR` reads the same clip |
| `getPrimaryClipDescription` | ok | a real `android.content.ClipDescription` materialised from the stored facts |
| `hasPrimaryClip` | ok | `true` iff a clip is stored |
| `hasClipboardText` | ok | facts: item 0 has text |
| `getPrimaryClipSource` | ok | package that set it |
| `clearPrimaryClip` | ok | clears and notifies the listeners |
| `add/removePrimaryClipChangedListener` | ok | listener set, idempotent |
| `areClipboardAccessNotificationsEnabledForUser` | ok | `false` |
| `setClipboardAccessNotificationsEnabledForUser` | ok | recorded |

Round trip proved in `tests/services_t38.rs`: bytes in → identical bytes out,
plus the semantic queries. `getPrimaryClipSource`-style scalar transactions run
through the decoded table; the two `ClipData` transactions are refused on the
VM-free path (structured error) because only `ClipData.CREATOR` may read them.

## Declared small services (`acos-services::declared`)

| service / transaction | status | reply |
|---|---|---|
| `IAppOpsService.checkOperation` / `checkOperationRaw` | ok | `MODE_ALLOWED` (0), call recorded |
| `IAppOpsService.checkAudioOperation` / `checkPackage` | ok | `MODE_ALLOWED` |
| `IAppOpsService.noteOperation` / `startOperation` / `noteProxyOperation` / `startProxyOperation` | unsupported | recorded, then structured error (Android would record a note) |
| `IAppOpsService.setMode` / `setUidMode` | unsupported | recorded, structured error |
| `IAppOpsService.finishOperation` / `finishProxyOperation` | ok | no reply |
| `IPermissionManager.getPermissionFlags` | ok | `0` |
| `IPermissionManager.shouldShowRequestPermissionRationale` / `isPermissionRevokedByPolicy` | ok | `false` |
| `IPermissionManager.grantRuntimePermission` / `revokeRuntimePermission` / `updatePermissionFlags` | unsupported | recorded, structured error |
| `IPermissionManager.getAllowlistedRestrictedPermissions` / auto-revoke lists | ok | empty list |
| `IUserManager.getUserInfo/getUsers/getProfiles/getPrimaryUser` | ok | a real `android.content.pm.UserInfo` materialised by `acos-runtime::user_java`; single user `0`, name `ACOS`, unknown id → `null` |
| `IUserManager.getProfileIds/getVisibleUsers` | ok | `int[]` of the single user `0` |
| `IUserManager.getMainUserId/getBootUser/getUserName/isUserUnlocked/isUserRunning/...` | ok | user 0 / `ACOS` / `true` |
| `IUserManager.createUserWithThrow/preCreateUserWithThrow/removeUser/setUserName/setUserIcon/setUserEnabled/setUserRestriction/markGuestForDeletion` | unsupported | recorded, structured error |
| `INotificationManager.areNotificationsEnabled(ForPackage)` / `canShowBadge` / `hasSentValidMsg` | ok | `false` |
| `INotificationManager.getPackageImportance` / `getZenMode` / `getHintsFromListenerNoToken` | ok | `0` |
| `INotificationManager.getActiveNotifications(WithAttribution)` / `getEnabledNotificationListeners` | ok | empty |
| `INotificationManager.enqueueNotificationWithTag` / `cancelNotificationWithTag` / `cancelAllNotifications` / `enqueueToast` / `enqueueTextToast` / `createNotificationChannels*` | unsupported | recorded, structured error (no status bar, no toast surface) |
| `IStatusBarService.expandNotificationsPanel/collapsePanels/togglePanel/expandSettingsPanel/setIcon/setIconVisibility/removeIcon/disable*/onPanel*` | ok | recorded no-ops |
| `IStatusBarService.setImeWindowStatus` | ok | recorded `(token, visibility)` |
| `IStatusBarService.getDisableFlags/isTracing/getLastSystemKey/getNavBarMode` | ok | zeros / `false` |
| `IStatusBarService.shutdown/reboot/restart/showAuthenticationDialog/tiles/biometrics/...` | unsupported | structured error |
| `IConnectivityManager.getProxyForNetwork` / `getGlobalProxy` | ok | `null` `ProxyInfo` (matches `services.tsv`) |
| `IConnectivityManager.getActiveNetwork(Info)` / `getAllNetworks` / `getDefaultRequest` | ok | `null` / empty |
| `IConnectivityManager.isActiveNetworkMetered/shouldAvoidBadWifi/isDefaultNetworkActive` | ok | `false` |
| `IConnectivityManager.reportInetCondition/reportNetworkConnectivity` | ok | no reply |
| `IConnectivityManager.requestNetwork/listenForNetwork/setGlobalProxy/...` | unsupported | structured error |

## Already present before W30-A (unchanged, still green)

`IAccessibilityManager` (addClient/getRecommendedTimeoutMillis/getFocusStrokeWidth/
getFocusColor/removeClient), `IInputManager` (device list/`getInputDevice`/
listener), `ITextServicesManager` (no spell checker), `IAutoFillManager`
(disabled + receiver answered), `IAudioService` (silent), `IGraphicsStats`
(memfd), `INetworkManagementService`, `IGameManagerService`,
`IVirtualDeviceManager`, `IContentCaptureManager`, `IVoiceInteractionManagerService`,
`IAndroidFuture`.

## Test evidence

```
cargo test -p acos-runtime --test services_t38          # 13 passed
cargo test -p acos-runtime --test services_t38_java     # 1 passed (real Stub$Proxy)
cargo test -p acos-services                              # 25 lib + host/replay
cargo clippy -p acos-services -p acos-runtime --all-targets -- -D warnings  # clean
```

## Java marshallers (T-38 follow-up)

The replies whose layout only the framework's own `writeToParcel` may define are
now materialised through the Java bridge in `acos-runtime`:

* `ime_java.rs` — `InputBindResult` (reads `NO_IME.result` off the real
  constant, sets the fields, lets `writeToParcel` emit). The request's real
  `EditorInfo`/`ImeOnBackInvokedDispatcher` are decoded with their own
  `CREATOR`s (`parcel_java.rs`).
* `clipboard_java.rs` — incoming `ClipData` decode + stored-byte re-emit,
  `ClipDescription` materialisation, and the
  `IOnPrimaryClipChangedListener.dispatchPrimaryClipChanged()` oneway delivery.
* `user_java.rs` — `UserInfo` (single-object and `List<UserInfo>` replies).

The VM-free path still refuses these replies with a structured error (the
`services_t38.rs` assertions), because only the wire path has a VM to marshal
with. The Java-level acceptance is `services_t38_java.rs` +
`tests/java/ImeClipboardMain.java`: the real `IInputMethodManager$Stub$Proxy`
reads `InputBindResult.NO_IME`, the real `IClipboard$Stub$Proxy` round-trips
`ClipData.newPlainText("l", "hello")`, the listener fires once per change, and
the real `IUserManager$Stub$Proxy` reads `UserInfo`.

## Open gaps (honest)

1. `IBatteryStats` (`com.android.internal.app.IBatteryStats`) and
   `ISensorPrivacyManager` (`android.hardware.ISensorPrivacyManager`) are decoded
   but not registered — no state machine yet.
2. `IInputMethodManager.showSoftInput`/`hideSoftInput` still carry a real
   `ResultReceiver` argument the generic codec has no spec for; the null-IME
   reply is `false` but the *request* decode would need the same `CREATOR`
   treatment. The measured launch/interaction path calls
   `startInputOrWindowGainedFocus`, which is now Java-marshalled.
3. The `every_framework_exception_code_round_trips` failure introduced by the
   T-38 commit (`write_exception` now writes the full AOSP body for every
   non-zero code, but the test still asserted a body for `NO_EXCEPTION`) is
   fixed in `exception.rs`: the test now asserts `writeException(0)` writes no
   body. The cause was the T-38 `exception.rs` change, **not** an AIDL table.
