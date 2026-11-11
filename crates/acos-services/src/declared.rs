//! The **declared** service set: interfaces an app can reach through
//! `ServiceManager` that the measured hello-views trace never calls.
//!
//! These exist because the launch trace is not the whole surface: the reference
//! apps only exercise the calls they make. Everything here is still answered
//! from the *mechanically decoded* `acos-aidl` tables (see
//! `acos_aidl::generate::DECLARED_INTERFACES`), never from a hand-typed code
//! table, and every unimplemented transaction is a structured
//! [`ServiceError::UnsupportedAndroidApi`] — no default success.
//!
//! Reply values:
//!
//! * `IAppOpsService.checkOperation` → `MODE_ALLOWED` (0) for a non-restricted
//!   op, or for a permission-gated op whose runtime permission is present in
//!   the PMS manifest-grant set. A permission-gated op without a grant returns
//!   `MODE_ERRORED` (2), matching `AppOpsManager.checkOp` on a device where the
//!   permission is a hard restriction — never a default success.
//! * `IPermissionManager.getPermissionFlags` → 0, `shouldShowRequestPermission-
//!   Rationale` → false, and the runtime-permission mutators are **recorded**
//!   no-ops (they would change system state on Android, so they are reported as
//!   [`ServiceError::UnsupportedAndroidApi`] instead).
//! * `IUserManager` → the single ACOS user (`id = 0`, `UserHandle.USER_SYSTEM`,
//!   name `ACOS`). It is a real state machine: create/remove/rename are
//!   `UnsupportedAndroidApi`, not silent successes.
//! * `INotificationManager` → notifications are **not displayed** (there is no
//!   status bar). `enqueue*`/`cancel*` are recorded and report
//!   `UnsupportedAndroidApi`; the read-only queries answer "nothing enabled".
//! * `IStatusBarService` → minimal: panel/icon/session transactions are
//!   recorded no-ops, everything else is unsupported.
//! * `IConnectivityManager.getProxyForNetwork` → `null` `ProxyInfo` (no proxy),
//!   the exact reply `services.tsv` records for the launch call.

use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;
use acos_parcel::binder::BinderId;

use crate::error::{ServiceError, Result};
use crate::host::{CallResult, ServiceObject};
use crate::value::Value;

/// `AppOpsManager.MODE_ALLOWED`.
pub const MODE_ALLOWED: i32 = 0;
/// `AppOpsManager.MODE_IGNORED`.
pub const MODE_IGNORED: i32 = 1;
/// `AppOpsManager.MODE_ERRORED`.
pub const MODE_ERRORED: i32 = 2;

/// `PackageManager.PERMISSION_GRANTED`.
pub const PERMISSION_GRANTED: i32 = 0;
/// `PackageManager.PERMISSION_DENIED`.
pub const PERMISSION_DENIED: i32 = -1;

/// The ACOS user id (`UserHandle.USER_SYSTEM`).
pub const USER_SYSTEM: i32 = 0;

fn unsupported(interface: &str, method: &MethodInfo) -> ServiceError {
    ServiceError::UnsupportedAndroidApi {
        interface: interface.to_string(),
        code: method.code,
        method: method.name.clone(),
    }
}

fn int_arg(args: &[Value], i: usize) -> i32 {
    args.get(i).and_then(Value::as_int).unwrap_or(0)
}

fn str_arg(args: &[Value], i: usize) -> String {
    args.get(i).and_then(Value::as_str).unwrap_or_default().to_string()
}

// ---------------------------------------------------------------------------
// IAppOpsService
// ---------------------------------------------------------------------------

/// `IAppOpsService` state: the recorded app-op calls, per `(uid, op, package)`.
#[derive(Debug, Default)]
pub struct AppOpsState {
    /// `checkOperation` calls: `(code, uid, package)`.
    pub checks: Vec<(i32, i32, String)>,
    /// `noteOperation`/`startOperation` calls (recorded, not applied).
    pub notes: Vec<(i32, i32, String)>,
    /// Explicit `setMode`/`setUidMode` requests.
    pub mode_changes: Vec<(i32, i32, i32, String)>,
    /// Runtime permissions granted to the app by its manifest / install (`-g`).
    /// Seeded from PMS state; an empty set means `checkOperation` denies every
    /// permission-gated op.
    pub granted_permissions: Vec<String>,
}

impl AppOpsState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The runtime permission that gates `code`, if the op is restricted.
    /// Mirrors the API-34 `AppOpsManager.sOpPerms` entries the launch path can
    /// reach.
    fn required_permission(code: i32) -> Option<&'static str> {
        Some(match code {
            0 => "android.permission.ACCESS_COARSE_LOCATION",
            1 | 2 => "android.permission.ACCESS_FINE_LOCATION",
            4 => "android.permission.READ_CONTACTS",
            5 => "android.permission.WRITE_CONTACTS",
            6 => "android.permission.READ_CALL_LOG",
            7 => "android.permission.WRITE_CALL_LOG",
            8 => "android.permission.READ_CALENDAR",
            9 => "android.permission.WRITE_CALENDAR",
            13 => "android.permission.CALL_PHONE",
            14 => "android.permission.READ_SMS",
            15 => "android.permission.WRITE_SMS",
            16..=22 => "android.permission.READ_SMS",
            23 => "android.permission.WRITE_SETTINGS",
            24 => "android.permission.SYSTEM_ALERT_WINDOW",
            26 => "android.permission.CAMERA",
            27 => "android.permission.RECORD_AUDIO",
            29 => "android.permission.READ_CLIPBOARD",
            30 => "android.permission.WRITE_CLIPBOARD",
            41 => "android.permission.BODY_SENSORS",
            56 => "android.permission.BODY_SENSORS",
            59 => "android.permission.READ_EXTERNAL_STORAGE",
            60 => "android.permission.WRITE_EXTERNAL_STORAGE",
            62 => "android.permission.READ_PHONE_STATE",
            _ => return None,
        })
    }

    /// The mode Android would return for `code`: `MODE_ALLOWED` for a benign or
    /// granted op, `MODE_ERRORED` for a permission-gated op without a grant.
    fn mode_for(&self, code: i32) -> i32 {
        match Self::required_permission(code) {
            Some(perm) if self.granted_permissions.iter().any(|p| p == perm) => MODE_ALLOWED,
            Some(_) => MODE_ERRORED,
            None => MODE_ALLOWED,
        }
    }
}

/// `IAppOpsService` handler.
pub struct AppOpsService {
    state: Arc<Mutex<AppOpsState>>,
}

impl AppOpsService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<AppOpsState>>) -> Self {
        Self { state }
    }

    /// Record a manifest/install permission grant for the app process (PMS).
    pub fn grant_permission(&mut self, permission: impl Into<String>) {
        self.state
            .lock()
            .unwrap()
            .granted_permissions
            .push(permission.into());
    }

    /// The current grant set, exposed so the boot report and tests can inspect
    /// the app-op decision input instead of assuming a default allow.
    #[must_use]
    pub fn granted_permissions(&self) -> Vec<String> {
        self.state.lock().unwrap().granted_permissions.clone()
    }
}

impl ServiceObject for AppOpsService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "com.android.internal.app.IAppOpsService" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            "checkOperation" => {
                let code = int_arg(args, 0);
                let uid = int_arg(args, 1);
                let package = str_arg(args, 2);
                let mut st = self.state.lock().unwrap();
                st.checks.push((code, uid, package));
                let mode = st.mode_for(code);
                vec![Value::Int(mode)]
            }
            "checkOperationRaw" => {
                let code = int_arg(args, 0);
                let uid = int_arg(args, 1);
                let package = str_arg(args, 2);
                let mut st = self.state.lock().unwrap();
                st.checks.push((code, uid, package));
                let mode = st.mode_for(code);
                vec![Value::Int(mode)]
            }
            "checkAudioOperation" => {
                let code = int_arg(args, 0);
                let st = self.state.lock().unwrap();
                vec![Value::Int(st.mode_for(code))]
            }
            "checkPackage" => {
                let _ = (int_arg(args, 0), str_arg(args, 1));
                vec![Value::Int(MODE_ALLOWED)]
            }
            // State-changing calls: Android would record/mutate. ACOS refuses
            // them explicitly instead of pretending they happened.
            "noteOperation" | "startOperation" | "noteProxyOperation" | "startProxyOperation" => {
                let code = int_arg(args, 1);
                let uid = int_arg(args, 2);
                let package = str_arg(args, 3);
                self.state
                    .lock()
                    .unwrap()
                    .notes
                    .push((code, uid, package));
                return Err(unsupported(interface, method))
            }
            "finishOperation" | "finishProxyOperation" => Vec::new(),
            "setMode" => {
                let code = int_arg(args, 0);
                let uid = int_arg(args, 1);
                let package = str_arg(args, 2);
                let mode = int_arg(args, 3);
                self.state
                    .lock()
                    .unwrap()
                    .mode_changes
                    .push((code, uid, mode, package));
                return Err(unsupported(interface, method))
            }
            "setUidMode" => {
                let code = int_arg(args, 0);
                let uid = int_arg(args, 1);
                let mode = int_arg(args, 2);
                self.state
                    .lock()
                    .unwrap()
                    .mode_changes
                    .push((code, uid, mode, String::new()));
                return Err(unsupported(interface, method))
            }
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IPermissionManager
// ---------------------------------------------------------------------------

/// `IPermissionManager` state (nothing granted at runtime; manifest
/// permissions are answered by the PMS).
#[derive(Debug, Default)]
pub struct PermissionState {
    /// Recorded runtime-permission mutations.
    pub mutations: Vec<(String, String, i32)>,
}

impl PermissionState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IPermissionManager` handler.
pub struct PermissionService {
    state: Arc<Mutex<PermissionState>>,
}

impl PermissionService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<PermissionState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for PermissionService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.permission.IPermissionManager" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            "getPermissionFlags" => vec![Value::Int(0)],
            "shouldShowRequestPermissionRationale" => vec![Value::Bool(false)],
            "isPermissionRevokedByPolicy" => vec![Value::Bool(false)],
            "getAllowlistedRestrictedPermissions" => vec![Value::List(Some(Vec::new()))],
            "getAutoRevokeExemptionRequestedPackages"
            | "getAutoRevokeExemptionGrantedPackages"
            | "getEnabledNotificationListenerPackages" => vec![Value::List(Some(Vec::new()))],
            "isAutoRevokeExempted" => vec![Value::Bool(false)],
            "grantRuntimePermission" | "revokeRuntimePermission" => {
                let package = str_arg(args, 0);
                let permission = str_arg(args, 1);
                let user = int_arg(args, 2);
                self.state
                    .lock()
                    .unwrap()
                    .mutations
                    .push((package, permission, user));
                return Err(unsupported(interface, method))
            }
            "updatePermissionFlags" | "updatePermissionFlagsForAllApps" => {
                let package = str_arg(args, 0);
                let permission = str_arg(args, 1);
                let user = int_arg(args, 4);
                self.state
                    .lock()
                    .unwrap()
                    .mutations
                    .push((package, permission, user));
                return Err(unsupported(interface, method))
            }
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IUserManager
// ---------------------------------------------------------------------------

/// `IUserManager` state: exactly one user (id 0, `ACOS`).
#[derive(Debug)]
pub struct UserState {
    /// `UserInfo.id` of the only user (0 = `USER_SYSTEM`).
    pub user_id: i32,
    /// User name.
    pub name: String,
    /// Recorded create/remove/rename requests (all unsupported).
    pub mutations: Vec<String>,
}

impl UserState {
    /// The single-user ACOS state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            user_id: USER_SYSTEM,
            name: "ACOS".to_string(),
            mutations: Vec::new(),
        }
    }

    fn user_info(&self) -> Value {
        // `android.content.pm.UserInfo.writeToParcel`: id, name, flags, iconPath,
        // (optional) userType, (optional) restricted profile parent.
        Value::TypedObject(crate::value::Parcelable::ordered(
            "android.content.pm.UserInfo",
            vec![
                Value::Int(self.user_id),
                Value::Str8(Some(self.name.clone())),
                Value::Int(0),
                Value::Str8(None),
            ],
        ))
    }
}

impl Default for UserState {
    fn default() -> Self {
        Self::new()
    }
}

/// `IUserManager` handler.
pub struct UserService {
    state: Arc<Mutex<UserState>>,
}

impl UserService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<UserState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for UserService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.os.IUserManager" {
            return Err(unsupported(interface, method));
        }
        let mut state = self.state.lock().unwrap();
        let reply = match method.name.as_str() {
            "getUserInfo" | "getProfileParent" | "getPrimaryUser" | "getProfileParentId" => {
                if method.name == "getProfileParentId" {
                    vec![Value::Int(state.user_id)]
                } else {
                    vec![state.user_info()]
                }
            }
            "getUsers" | "getProfiles" => vec![Value::List(Some(vec![state.user_info()]))],
            "getProfileIds" => {
                let user = int_arg(args, 0);
                if user == state.user_id {
                    vec![Value::IntArray(Some(vec![state.user_id]))]
                } else {
                    vec![Value::IntArray(Some(Vec::new()))]
                }
            }
            "getMainUserId" | "getBootUser" => {
                vec![Value::Int(state.user_id)]
            }
            "getVisibleUsers" => vec![Value::IntArray(Some(vec![state.user_id]))],
            "getUserName" => vec![Value::Str(Some(state.name.clone()))],
            // Only the one real user (id 0) is unlocked/running/foreground. An
            // unknown id must not be reported as a healthy user: Android
            // returns `false` (or throws) rather than a blanket `true`.
            "isUserUnlocked" | "isUserRunning" | "isUserForeground" | "isUserVisible"
            | "isUserUnlockingOrUnlocked" | "isUserNameSet" | "isDemoUser" | "isAdminUser" => {
                let user = int_arg(args, 0);
                vec![Value::Bool(user == state.user_id)]
            }
            "isSameProfileGroup" => {
                let a = int_arg(args, 0);
                let b = int_arg(args, 1);
                vec![Value::Bool(a == state.user_id && b == state.user_id)]
            }
            "isRestricted" | "canHaveRestrictedProfile" | "isHeadlessSystemUserMode"
            | "isUserSwitcherEnabled" | "hasRestrictedProfiles" | "hasBadge" => {
                vec![Value::Bool(false)]
            }
            "getUserRestrictions" | "getApplicationRestrictions" => {
                vec![Value::Bundle(None)]
            }
            "hasUserRestriction" | "hasBaseUserRestriction" => vec![Value::Bool(false)],
            "getUserSerialNumber" | "getUserHandle" | "getCredentialOwnerProfile" => {
                vec![Value::Int(state.user_id)]
            }
            "getUserCreationTime" | "getUserStartRealtime" | "getUserUnlockRealtime" => {
                vec![Value::Long(0)]
            }
            "createUserWithThrow" | "preCreateUserWithThrow" | "removeUser" | "setUserName"
            | "setUserIcon" | "setUserEnabled" | "setUserRestriction" | "markGuestForDeletion" => {
                state.mutations.push(method.name.clone());
                return Err(unsupported(interface, method))
            }
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IUiModeManager
// ---------------------------------------------------------------------------

/// `UiModeManager` mode constants the framework reads back.
pub const UI_MODE_TYPE_NORMAL: i32 = 1;
/// `UiModeManager.MODE_NIGHT_NO` (night mode off).
pub const MODE_NIGHT_NO: i32 = 1;

/// `IUiModeManager` state: the ACOS device is a normal (non-car/desk/tv)
/// phone in day mode, matching the measured device config. There is no
/// contrast controller, so `addCallback`/`removeCallback` are recorded and the
/// mutators report a structured unsupported error rather than a fake success.
#[derive(Debug, Default)]
pub struct UiModeState {
    /// Night-mode mutators that Android would have applied.
    pub mutations: Vec<String>,
}

impl UiModeState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IUiModeManager` handler.
pub struct UiModeService {
    state: Arc<Mutex<UiModeState>>,
}

impl UiModeService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<UiModeState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for UiModeService {
    fn call(&mut self, interface: &str, method: &MethodInfo, _args: &[Value]) -> Result<CallResult> {
        if interface != "android.app.IUiModeManager" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            // The ACOS reference device is a normal handheld in day mode.
            "getCurrentModeType" => vec![Value::Int(UI_MODE_TYPE_NORMAL)],
            "getNightMode" => vec![Value::Int(MODE_NIGHT_NO)],
            // No active contrast controller: the neutral value is 0.0f.
            "getContrast" => vec![Value::Float(0.0)],
            "isNightModeLocked" | "isUiModeLocked" | "isUiModeLockedForUser" => {
                vec![Value::Bool(false)]
            }
            // The real `UiModeManager` constructor registers its contrast
            // callback. ACOS has no contrast controller, so the callback is
            // recorded and never invoked: a no-op success, not a fake value.
            "addCallback" | "removeCallback" => {
                self.state.lock().unwrap().mutations.push(method.name.clone());
                vec![]
            }
            // Mutators and the contrast-listener registration are not
            // implemented: report them honestly instead of a no-op success.
            "setNightMode" | "setApplicationNightMode" | "setNightModeForUser"
            | "setNightModeForUserWithDisplay" | "enableCarMode" | "disableCarMode"
            | "setDesktopMode" | "setCarMode"
            | "addContrastChangeListener" | "removeContrastChangeListener"
            | "setCustomNightModeStart" | "setCustomNightModeEnd" => {
                self.state.lock().unwrap().mutations.push(method.name.clone());
                return Err(unsupported(interface, method));
            }
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// INotificationManager
// ---------------------------------------------------------------------------

/// `INotificationManager` state: recording only, nothing is ever posted.
#[derive(Debug, Default)]
pub struct NotificationState {
    /// Notifications that Android would have posted.
    pub posted: Vec<(String, i32)>,
    /// Toasts that Android would have shown.
    pub toasts: Vec<String>,
    /// Channels created (recorded).
    pub channels: Vec<String>,
}

impl NotificationState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `INotificationManager` handler.
pub struct NotificationService {
    state: Arc<Mutex<NotificationState>>,
}

impl NotificationService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<NotificationState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for NotificationService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.app.INotificationManager" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            "areNotificationsEnabledForPackage" => vec![Value::Bool(false)],
            "areNotificationsEnabled" => vec![Value::Bool(false)],
            "canShowBadge" | "hasSentValidMsg" | "isInInvalidMsgState" => vec![Value::Bool(false)],
            "getPackageImportance" => vec![Value::Int(0)],
            "getActiveNotifications" | "getActiveNotificationsWithAttribution" => {
                vec![Value::List(Some(Vec::new()))]
            }
            "getNotificationChannels" | "getNotificationChannelsForPackage" => {
                vec![Value::TypedObject(crate::value::Parcelable::Null)]
            }
            "getHintsFromListenerNoToken" => vec![Value::Int(0)],
            "getZenMode" => vec![Value::Int(0)],
            "isNotificationListenerAccessGranted" => vec![Value::Bool(false)],
            "getEnabledNotificationListeners" => vec![Value::List(Some(Vec::new()))],
            // Android would post/clear/alter notification state. ACOS has no
            // status bar, so this is recorded and reported as unsupported.
            "enqueueNotificationWithTag" => {
                let package = str_arg(args, 0);
                let id = int_arg(args, 3);
                self.state.lock().unwrap().posted.push((package, id));
                return Err(unsupported(interface, method))
            }
            "cancelNotificationWithTag" | "cancelAllNotifications" => {
                let package = str_arg(args, 0);
                self.state.lock().unwrap().posted.push((package, 0));
                return Err(unsupported(interface, method))
            }
            "enqueueToast" | "enqueueTextToast" => {
                let package = str_arg(args, 0);
                self.state.lock().unwrap().toasts.push(package);
                return Err(unsupported(interface, method))
            }
            "createNotificationChannels" | "createNotificationChannelsForPackage" => {
                let package = str_arg(args, 0);
                self.state.lock().unwrap().channels.push(package);
                return Err(unsupported(interface, method))
            }
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IStatusBarService (minimal)
// ---------------------------------------------------------------------------

/// `IStatusBarService` state: panels/IME/icon transactions are recorded no-ops.
#[derive(Debug, Default)]
pub struct StatusBarState {
    /// Method names of the recorded calls, in order.
    pub calls: Vec<String>,
    /// `setImeWindowStatus` visibility values.
    pub ime_status: Vec<(BinderId, i32)>,
}

impl StatusBarState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IStatusBarService` handler.
pub struct StatusBarService {
    state: Arc<Mutex<StatusBarState>>,
}

impl StatusBarService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<StatusBarState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for StatusBarService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "com.android.internal.statusbar.IStatusBarService" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            "expandNotificationsPanel" | "collapsePanels" | "togglePanel"
            | "expandSettingsPanel" | "setIcon" | "setIconVisibility" | "removeIcon"
            | "onPanelRevealed" | "onPanelHidden" | "clearNotificationEffects"
            | "disable" | "disableForUser" | "disable2" | "disable2ForUser" => {
                self.state.lock().unwrap().calls.push(method.name.clone());
                Vec::new()
            }
            "setImeWindowStatus" => {
                let token = args.first().and_then(Value::as_binder).unwrap_or(BinderId(0));
                let visibility = int_arg(args, 1);
                self.state
                    .lock()
                    .unwrap()
                    .ime_status
                    .push((token, visibility));
                Vec::new()
            }
            "getDisableFlags" => vec![Value::IntArray(Some(vec![0, 0]))],
            "isTracing" => vec![Value::Bool(false)],
            "getLastSystemKey" => vec![Value::Int(0)],
            "getNavBarMode" => vec![Value::Int(0)],
            // System-level effects (shutdown, reboot, biometric dialogs, tiles):
            // ACOS cannot perform them; report unsupported rather than success.
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IConnectivityManager
// ---------------------------------------------------------------------------

/// `IConnectivityManager` state: no network, no proxy.
#[derive(Debug, Default)]
pub struct ConnectivityState {
    /// Recorded proxy lookups (`Network` presence marker).
    pub proxy_lookups: u32,
}

impl ConnectivityState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IConnectivityManager` handler.
pub struct ConnectivityService {
    state: Arc<Mutex<ConnectivityState>>,
}

impl ConnectivityService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<ConnectivityState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for ConnectivityService {
    fn call(&mut self, interface: &str, method: &MethodInfo, _args: &[Value]) -> Result<CallResult> {
        if interface != "android.net.IConnectivityManager" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            // `services.tsv`: launch call #1, reply `null` (no global proxy).
            "getProxyForNetwork" | "getGlobalProxy" => {
                self.state.lock().unwrap().proxy_lookups += 1;
                vec![Value::TypedObject(crate::value::Parcelable::Null)]
            }
            "getActiveNetwork" => vec![Value::TypedObject(crate::value::Parcelable::Null)],
            "getActiveNetworkInfo" => {
                vec![Value::TypedObject(crate::value::Parcelable::Null)]
            }
            "getAllNetworks" => vec![Value::List(Some(Vec::new()))],
            "getAllNetworkInfo" => vec![Value::List(Some(Vec::new()))],
            "isActiveNetworkMetered" | "shouldAvoidBadWifi" | "isDefaultNetworkActive" => {
                vec![Value::Bool(false)]
            }
            "getMultipathPreference" => vec![Value::Int(0)],
            "getRestrictBackgroundStatusByCaller" => vec![Value::Int(0)],
            "getCaptivePortalServerUrl" => vec![Value::Str(Some(String::new()))],
            "getDefaultRequest" => vec![Value::TypedObject(crate::value::Parcelable::Null)],
            "reportInetCondition" | "reportNetworkConnectivity" => Vec::new(),
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}
