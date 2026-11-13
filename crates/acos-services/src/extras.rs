//! Services every Android process is guaranteed to resolve, and which a real
//! offline single-user device answers without asking anyone: storage, shortcuts,
//! jobs, power and accounts.
//!
//! These exist because an app dereferences `getSystemService(...)` without a
//! null check (Android never returns null for a published service). The ACOS
//! answer is the device's real state, not a fabricated success:
//!
//! * **storage** — one primary emulated volume at `/storage/emulated/0`, mounted,
//!   with no UUID, exactly what `StorageManagerService` builds for user 0 on an
//!   emulated device (`StorageVolume.Builder(...).setStorageUuid(null)`).
//! * **shortcuts** — no dynamic shortcuts exist yet, the per-activity limit is
//!   the platform's 5 for a normal app, and the mutators are accepted (the
//!   service stores them; nothing is rendered because ACOS has no launcher).
//! * **jobs** — no jobs are pending; `schedule` returns `RESULT_SUCCESS` and
//!   `cancel` is a no-op, matching `JobSchedulerService` for a fresh app.
//! * **power** — interactive, not in power save, no device-idle, wake locks are
//!   recorded and acquired as no-ops (the host never suspends).
//! * **accounts** — one user, no accounts installed, so every query is empty.
//!   Adding an account is reported unsupported (it would mutate device state).
//!
//! Everything else is a structured [`ServiceError::UnsupportedAndroidApi`].

use acos_aidl::MethodInfo;

use crate::error::{ServiceError, Result};
use crate::host::{CallResult, ServiceObject};
use crate::value::{Parcelable, Value};

/// `JobScheduler.RESULT_SUCCESS`.
pub const JOB_RESULT_SUCCESS: i32 = 1;
/// `JobScheduler.RESULT_FAILURE`.
pub const JOB_RESULT_FAILURE: i32 = 0;

/// `ShortcutManager`'s platform cap for a normal application
/// (`ShortcutService.getMaxShortcutCountPerActivity`).
pub const MAX_SHORTCUTS_PER_ACTIVITY: i32 = 5;

fn unsupported(interface: &str, method: &MethodInfo) -> ServiceError {
    ServiceError::UnsupportedAndroidApi {
        interface: interface.to_string(),
        code: method.code,
        method: method.name.clone(),
    }
}

fn str_arg(args: &[Value], i: usize) -> Option<String> {
    args.get(i).and_then(Value::as_str).map(str::to_string)
}

fn int_arg(args: &[Value], i: usize) -> i32 {
    args.get(i).and_then(Value::as_int).unwrap_or(0)
}

fn empty_list() -> Value {
    Value::List(Some(Vec::new()))
}

fn empty_typed_array(class: &str) -> Value {
    let _ = class;
    Value::List(Some(Vec::new()))
}

fn typed_null() -> Value {
    Value::TypedObject(Parcelable::Null)
}

// ---------------------------------------------------------------------------
// IStorageManager ("mount")
// ---------------------------------------------------------------------------

/// The one emulated volume user 0 sees on an offline single-user device.
///
/// Built from the image's `StorageVolume(Parcel)` field order (the codec in
/// `parcelable.rs`): id, path, internal path, description, five flags, max file
/// size, owner `UserHandle`, nullable UUID, fsUuid, state. The UUID is `null`
/// because an emulated volume has none (`StorageManagerService` passes null for
/// `sPrimaryStorageUuid` on emulated devices); the state is `mounted`, the fs
/// label is absent.
#[must_use]
pub fn primary_emulated_volume() -> Value {
    Value::Parcelable(Parcelable::ordered(
        "android.os.storage.StorageVolume",
        vec![
            Value::Str8(Some("emulated;0".into())),
            Value::Str8(Some("/storage/emulated/0".into())),
            Value::Str8(Some("/storage/emulated/0".into())),
            Value::Str8(Some("Internal shared storage".into())),
            Value::Int(1), // primary
            Value::Int(0), // removable
            Value::Int(1), // emulated
            Value::Int(0), // externallyManaged
            Value::Int(0), // allowMassStorage
            Value::Long(0),
            Value::NamedParcelable(Parcelable::ordered(
                "android.os.UserHandle",
                vec![Value::Int(0)],
            )),
            Value::Cond {
                present: false,
                values: Vec::new(),
            },
            Value::Str8(None), // fsUuid
            Value::Str8(Some("mounted".into())),
        ],
    ))
}

/// `IStorageManager` handler.
#[derive(Debug, Default)]
pub struct StorageService;

impl StorageService {
    /// New handler.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ServiceObject for StorageService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.os.storage.IStorageManager" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            // `StorageManager.getVolumeList` is called on almost every app
            // start. A real device returns exactly one primary emulated volume
            // for user 0; `getPrimaryStorageVolume()[0]` would NPE on an empty
            // array, so the array is never empty here.
            "getVolumeList" => {
                let _ = (int_arg(args, 0), str_arg(args, 1), int_arg(args, 2));
                vec![Value::List(Some(vec![primary_emulated_volume()]))]
            }
            "getPrimaryStorageUuid" => vec![Value::Str(Some("primary_physical".into()))],
            "getDisks" | "getVolumes" | "getVolumeRecords" => {
                vec![empty_typed_array("android.os.storage.VolumeInfo")]
            }
            "lastMaintenance" => vec![Value::Long(0)],
            "isObbMounted" => vec![Value::Bool(false)],
            "getMountedObbPath" => vec![Value::Str(None)],
            // A real vold would validate the path and create it. ACOS has no
            // per-app storage sandbox to validate against; the framework calls
            // this only for directories it is about to use.
            "mkdirs" | "fixupAppDir" => Vec::new(),
            "registerListener" | "unregisterListener" => Vec::new(),
            // Privileged device mutations: report honestly instead of a
            // fabricated success.
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IThermalService ("thermalservice")
// ---------------------------------------------------------------------------

/// `IThermalService` handler: a cool device. `PowerManager`'s constructor
/// fetches this binder, so a null here fails resolution of `PowerManager`
/// itself (`com.fsck.k9`'s Koin graph).
#[derive(Debug, Default)]
pub struct ThermalService;

impl ThermalService {
    /// New handler.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ServiceObject for ThermalService {
    fn call(&mut self, interface: &str, method: &MethodInfo, _args: &[Value]) -> Result<CallResult> {
        if interface != "android.os.IThermalService" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            // `PowerManager.THERMAL_STATUS_NONE`.
            "getCurrentThermalStatus" => vec![Value::Int(0)],
            "getThermalHeadroom" => vec![Value::Float(1.0)],
            "getCurrentTemperatures" | "getCurrentTemperaturesWithType" => vec![
                Value::List(Some(Vec::new())),
            ],
            "getCurrentCoolingDevices" | "getCurrentCoolingDevicesWithType" => {
                vec![Value::List(Some(Vec::new()))]
            }
            "registerThermalEventListener" | "registerThermalEventListenerWithType"
            | "unregisterThermalEventListener" | "registerThermalStatusListener"
            | "unregisterThermalStatusListener" => vec![Value::Bool(true)],
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IAlarmManager ("alarm")
// ---------------------------------------------------------------------------

/// `IAlarmManager` handler: alarms are accepted and recorded, but nothing wakes
/// the host (there is no suspend state). This is the same contract as a real
/// device whose app alarm is queued: `set` succeeds, `setTime` is a system
/// operation, and no alarm clock is currently displayed.
#[derive(Debug, Default)]
pub struct AlarmState {
    /// Pending alarms: `(package, type)`.
    pub alarms: Vec<(String, i32)>,
}

impl AlarmState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IAlarmManager` handler.
pub struct AlarmService {
    state: std::sync::Arc<std::sync::Mutex<AlarmState>>,
}

impl AlarmService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: std::sync::Arc<std::sync::Mutex<AlarmState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for AlarmService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.app.IAlarmManager" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            "set" => {
                let package = str_arg(args, 0).unwrap_or_default();
                let kind = int_arg(args, 1);
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .alarms
                    .push((package, kind));
                Vec::new()
            }
            "remove" | "removeAll" | "setTimeZone" => Vec::new(),
            "setTime" => vec![Value::Bool(true)],
            // No idle deadline is scheduled; `Long.MAX_VALUE` is the platform's
            // "no next wake from idle" value.
            "getNextWakeFromIdleTime" => vec![Value::Long(i64::MAX)],
            "getNextAlarmClock" => vec![typed_null()],
            // An app's own alarms (including exact alarms) are schedulable on a
            // device with no battery-saver policy restricting them.
            "canScheduleExactAlarms" | "hasScheduleExactAlarm" => vec![Value::Bool(true)],
            "getConfigVersion" => vec![Value::Int(0)],
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IShortcutService ("shortcut")
// ---------------------------------------------------------------------------

/// `IShortcutService` state: the dynamic shortcuts an app published this
/// session. ACOS has no launcher, so nothing is rendered; the service still
/// keeps the platform's rate/limit answers honest.
#[derive(Debug, Default)]
pub struct ShortcutState {
    /// `(package, shortcut id)` pairs the app set.
    pub shortcuts: Vec<(String, String)>,
}

impl ShortcutState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IShortcutService` handler.
pub struct ShortcutService {
    state: std::sync::Arc<std::sync::Mutex<ShortcutState>>,
}

impl ShortcutService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: std::sync::Arc<std::sync::Mutex<ShortcutState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for ShortcutService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.content.pm.IShortcutService" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            "getMaxShortcutCountPerActivity" => vec![Value::Int(MAX_SHORTCUTS_PER_ACTIVITY)],
            // A fresh app has the full budget; the reset time is only
            // meaningful once the budget is exhausted.
            "getRemainingCallCount" => vec![Value::Int(MAX_SHORTCUTS_PER_ACTIVITY)],
            "getRateLimitResetTime" => vec![Value::Long(0)],
            // `ShortcutManager.getIconMaxWidth/Height` is 96 dp on a normal
            // device (`ShortcutService.mMaxIconDimension`).
            "getIconMaxDimensions" => vec![Value::Int(96)],
            "getBackupPayload" => vec![Value::ByteArray(Some(Vec::new()))],
            "getShareTargets" | "getShortcuts" => vec![Value::TypedObject(
                crate::parcelable::parceled_list_of(Vec::new()),
            )],
            "hasShareTargets" | "isRequestPinItemSupported" => vec![Value::Bool(false)],
            "setDynamicShortcuts" | "addDynamicShortcuts" | "updateShortcuts" => {
                let package = str_arg(args, 0).unwrap_or_default();
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(slice) = args.get(1).and_then(Value::as_parcelable) {
                    for item in crate::parcelable::parceled_list_items(slice).unwrap_or(&[]) {
                        if let Some(shortcut) = item.as_parcelable() {
                            // The shortcut's id is its first string field; a
                            // payload ACOS cannot read simply contributes no id.
                            if let Some(id) = shortcut
                                .field(0)
                                .and_then(Value::as_str)
                                .map(str::to_string)
                            {
                                st.shortcuts.push((package.clone(), id));
                            }
                        }
                    }
                }
                // `ShortcutService` accepts the call on a fresh app; an app
                // that exceeds the cap gets `false`, which ACOS does not model
                // (it has no rate limiter yet).
                vec![Value::Bool(true)]
            }
            "removeDynamicShortcuts" | "removeAllDynamicShortcuts" | "removeLongLivedShortcuts"
            | "enableShortcuts" => {
                let package = str_arg(args, 0).unwrap_or_default();
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                match method.name.as_str() {
                    "removeAllDynamicShortcuts" => {
                        st.shortcuts.retain(|(pkg, _)| pkg != &package);
                    }
                    _ => {
                        let ids: Vec<String> = args
                            .get(1)
                            .and_then(|v| match v {
                                Value::StringList(Some(list)) => Some(
                                    list.iter()
                                        .filter_map(|s| s.clone())
                                        .collect::<Vec<_>>(),
                                ),
                                _ => None,
                            })
                            .unwrap_or_default();
                        st.shortcuts.retain(|(pkg, id)| {
                            pkg != &package || !ids.iter().any(|wanted| wanted == id)
                        });
                    }
                }
                Vec::new()
            }
            "reportShortcutUsed" | "resetThrottling" | "onApplicationActive"
            | "applyRestore" => Vec::new(),
            // Pinning needs a launcher UI; the platform reports false for a
            // device without a pinning-capable launcher.
            "requestPinShortcut" | "createShortcutResultIntent" | "disableShortcuts" => {
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
// IJobScheduler ("jobscheduler")
// ---------------------------------------------------------------------------

/// `IJobScheduler` state: accepted jobs this session.
#[derive(Debug, Default)]
pub struct JobState {
    /// `(package, job id)` pairs the app scheduled and has not cancelled.
    pub jobs: Vec<(String, i32)>,
}

impl JobState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IJobScheduler` handler.
pub struct JobSchedulerService {
    state: std::sync::Arc<std::sync::Mutex<JobState>>,
}

impl JobSchedulerService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: std::sync::Arc<std::sync::Mutex<JobState>>) -> Self {
        Self { state }
    }
}

fn job_id(args: &[Value]) -> i32 {
    args.get(1)
        .and_then(Value::as_parcelable)
        .and_then(|p| p.field(0))
        .and_then(Value::as_int)
        .unwrap_or(0)
}

impl ServiceObject for JobSchedulerService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.app.job.IJobScheduler" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            // `JobSchedulerService.schedule` answers `RESULT_SUCCESS` for an
            // accepted job. ACOS has no job executor, so the job is recorded
            // and never runs; that is the honest offline answer, not a pass.
            "schedule" | "enqueue" | "scheduleAsPackage" => {
                let package = str_arg(args, 0).unwrap_or_default();
                let id = job_id(args);
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .jobs
                    .push((package, id));
                vec![Value::Int(JOB_RESULT_SUCCESS)]
            }
            "cancel" => {
                let package = str_arg(args, 0).unwrap_or_default();
                let id = int_arg(args, 1);
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .jobs
                    .retain(|(pkg, job)| pkg != &package || *job != id);
                Vec::new()
            }
            "cancelAll" => {
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .jobs
                    .clear();
                Vec::new()
            }
            "cancelAllInNamespace" => Vec::new(),
            // No pending job exists after a fresh start unless the app just
            // scheduled one; ACOS does not fabricate a `JobInfo` payload it
            // cannot round-trip, so an unknown id answers null exactly like a
            // device after a reboot.
            "getPendingJob" => vec![typed_null()],
            "getPendingJobReason" => vec![Value::Int(0)],
            "getAllPendingJobs" => vec![Value::Map(Some(Vec::new()))],
            "getAllPendingJobsInNamespace" | "getAllJobSnapshots" => vec![Value::TypedObject(
                crate::parcelable::parceled_list_of(Vec::new()),
            )],
            "getStartedJobs" => vec![empty_list()],
            "canRunUserInitiatedJobs" | "hasRunUserInitiatedJobsPermission" => {
                vec![Value::Bool(false)]
            }
            "registerUserVisibleJobObserver" | "unregisterUserVisibleJobObserver"
            | "notePendingUserRequestedAppStop" => Vec::new(),
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IPowerManager ("power")
// ---------------------------------------------------------------------------

/// `IPowerManager` handler: the power state of an awake, mains-powered,
/// non-idle device. Wake locks are recorded and never actually suspend the
/// host.
#[derive(Debug, Default)]
pub struct PowerState {
    /// Wake locks acquired by the app: `(tag, level_and_flags)`.
    pub wake_locks: Vec<(String, i32)>,
}

impl PowerState {
    /// New empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IPowerManager` handler.
pub struct PowerService {
    state: std::sync::Arc<std::sync::Mutex<PowerState>>,
}

impl PowerService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: std::sync::Arc<std::sync::Mutex<PowerState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for PowerService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.os.IPowerManager" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            "acquireWakeLock" | "acquireWakeLockAsync" => {
                let tag = str_arg(args, 2).unwrap_or_default();
                let level = int_arg(args, 1);
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .wake_locks
                    .push((tag, level));
                Vec::new()
            }
            "acquireWakeLockWithUid" => {
                let tag = str_arg(args, 2).unwrap_or_default();
                let level = int_arg(args, 1);
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .wake_locks
                    .push((tag, level));
                Vec::new()
            }
            "releaseWakeLock" | "releaseWakeLockAsync" | "updateWakeLockUids"
            | "updateWakeLockUidsAsync" | "updateWakeLockWorkSource" | "updateWakeLockCallback"
            | "boostScreenBrightness" | "userActivity" => Vec::new(),
            // `PowerManager.isWakeLockLevelSupported` on an always-awake host:
            // the partial/screen levels exist, doze/proximity do not.
            "isWakeLockLevelSupported" => {
                let level = int_arg(args, 0) & 0x0f;
                vec![Value::Bool(matches!(level, 1 | 6 | 10))]
            }
            "isInteractive" | "isDisplayInteractive" => vec![Value::Bool(true)],
            "isPowerSaveMode" | "areAutoPowerSaveModesEnabled" | "isDeviceIdleMode"
            | "isLightDeviceIdleMode" | "isLowPowerStandbySupported"
            | "isLowPowerStandbyEnabled" | "isExemptFromLowPowerStandby"
            | "isScreenBrightnessBoosted" | "isAmbientDisplayAvailable"
            | "isAmbientDisplaySuppressed" | "isBatteryDischargePredictionPersonalized" => {
                vec![Value::Bool(false)]
            }
            // Apps commonly ask whether they are exempt from battery
            // optimisations; on a device with no whitelist the answer is no.
            "isIgnoringBatteryOptimizations" => vec![Value::Bool(false)],
            "getBrightnessConstraint" => vec![Value::Float(0.0)],
            "getPowerSaveModeTrigger" | "getLastShutdownReason" | "getLastSleepReason" => {
                vec![Value::Int(0)]
            }
            "getPowerSaveState" => vec![typed_null()],
            "setPowerModeChecked" | "setPowerSaveModeEnabled" | "setDynamicPowerSaveHint"
            | "setAdaptivePowerSaveEnabled" => vec![Value::Bool(false)],
            "setPowerBoost" | "setPowerMode" | "setStayOnSetting" | "setDozeAfterScreenOff"
            | "suppressAmbientDisplay" | "setAttentionLight" | "nap" => Vec::new(),
            // Real device-state changes (sleep, reboot, shutdown, suspend) are
            // not something a guest may perform.
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IAccountManager ("account")
// ---------------------------------------------------------------------------

/// `IAccountManager` handler: one user, no accounts.
#[derive(Debug, Default)]
pub struct AccountService;

impl AccountService {
    /// New handler.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

fn empty_accounts() -> Value {
    empty_typed_array("android.accounts.Account")
}

impl ServiceObject for AccountService {
    fn call(&mut self, interface: &str, method: &MethodInfo, _args: &[Value]) -> Result<CallResult> {
        if interface != "android.accounts.IAccountManager" {
            return Err(unsupported(interface, method));
        }
        let reply = match method.name.as_str() {
            "getAuthenticatorTypes" => vec![empty_typed_array(
                "android.accounts.AuthenticatorDescription",
            )],
            "getAccountsForPackage" | "getAccountsByTypeForPackage" | "getAccountsAsUser" => {
                vec![empty_accounts()]
            }
            "getPassword" | "getUserData" | "peekAuthToken" | "getPreviousName" => {
                vec![Value::Str(None)]
            }
            "getAccountVisibility" => vec![Value::Int(0)],
            "someUserHasAccount" | "accountAuthenticated" | "hasAccountAccess" => {
                vec![Value::Bool(false)]
            }
            "invalidateAuthToken" | "setAuthToken" | "setPassword" | "clearPassword"
            | "setUserData" | "registerAccountListener" | "unregisterAccountListener"
            | "onAccountAccessed" => Vec::new(),
            // Adding/removing an account, and the callback-driven auth flows,
            // mutate device state or need an authenticator UI ACOS does not
            // have. Report them instead of pretending.
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IRestrictionsManager ("restrictions")
// ---------------------------------------------------------------------------

/// Descriptor of `android.content.IRestrictionsManager`.
pub const RESTRICTIONS_DESCRIPTOR: &str = "android.content.IRestrictionsManager";
const RESTRICTIONS_GET_APPLICATION: u32 = 1;
const RESTRICTIONS_GET_APPLICATION_PER_ADMIN: u32 = 2;
const RESTRICTIONS_HAS_PROVIDER: u32 = 3;
const RESTRICTIONS_REQUEST_PERMISSION: u32 = 4;
const RESTRICTIONS_NOTIFY_PERMISSION_RESPONSE: u32 = 5;

/// `android.content.IRestrictionsManager`: an unmanaged device.
///
/// `Context.getSystemService(Context.RESTRICTIONS_SERVICE)` returns a
/// `RestrictionsManager` on every device (the service always exists). ACOS
/// published none, so `getSystemService("restrictions")` returned null and
/// `com.tailscale.ipn`'s `App.initializeApp` died calling `.getClass()` on it
/// before its UI could start.
///
/// There is no device-owner/MDM on ACOS, which is exactly the state
/// `RestrictionsManagerService` reports when no restrictions provider is
/// configured: `hasRestrictionsProvider=false` and an **empty** `Bundle` of
/// application restrictions (Tailscale then uses its default
/// `HardwareAttestation=true` from `Bundle.getBoolean`'s default, unchanged).
#[derive(Debug, Default)]
pub struct RestrictionsService;

impl RestrictionsService {
    /// New handler.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ServiceObject for RestrictionsService {
    fn call(&mut self, interface: &str, method: &MethodInfo, _args: &[Value]) -> Result<CallResult> {
        Err(unsupported(interface, method))
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        _data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != RESTRICTIONS_DESCRIPTOR {
            return Ok(false);
        }
        match code {
            RESTRICTIONS_GET_APPLICATION => {
                // The image's proxy reads this reply with
                // `Parcel.readTypedObject(Bundle.CREATOR)` (not `readBundle`),
                // so the presence int comes first, then the Bundle payload.
                // Getting this wrong is visible in the app: presence read as 0
                // returns a **null** Bundle (`getBoolean` NPE) and a payload
                // without the presence marker makes `Bundle.CREATOR` read the
                // `BNDL` magic as a length ("Bundle length is not aligned").
                crate::exception::write_no_exception(reply);
                reply.write_int32(1);
                crate::parcelable::write_bundle(reply, Some(&crate::value::Bundle::new()))?;
                Ok(true)
            }
            RESTRICTIONS_HAS_PROVIDER => {
                crate::exception::write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            RESTRICTIONS_REQUEST_PERMISSION => {
                // A real service with no provider returns a null IntentSender
                // (`RestrictionsManagerService.requestPermission` logs and
                // returns null when `mRestrictionsProvider == null`).
                crate::exception::write_no_exception(reply);
                // `writeTypedObject(null, ...)`: the presence int is 0.
                reply.write_int32(0);
                Ok(true)
            }
            RESTRICTIONS_NOTIFY_PERMISSION_RESPONSE => {
                crate::exception::write_no_exception(reply);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

// ---------------------------------------------------------------------------
// ILocationManager ("location")
// ---------------------------------------------------------------------------

/// `ILocationManager.Stub.TRANSACTION_geocoderIsPresent` (image class
/// `android.location.ILocationManager$Stub`, constant value 12).
pub const LOCATION_TRANSACTION_GEOCODER_IS_PRESENT: u32 = 12;
/// Descriptor of the platform location service.
pub const LOCATION_DESCRIPTOR: &str = "android.location.ILocationManager";

/// `android.location.ILocationManager`: the service a real device always
/// publishes, with no location hardware and no geocoder provider.
///
/// `Geocoder.isPresent()` is a static helper that asks this binder
/// (`geocoderIsPresent`) without a null check:
///
/// ```text
/// ServiceManager.getService("location") -> null
///   -> Objects.requireNonNull(null) -> NullPointerException
/// ```
///
/// (measured from the API-34 `android.location.Geocoder` dex, line 73/74), which
/// is what killed `com.dot.gallery` in `GalleryApp.onCreate` after its keystore
/// wall was removed. A device without a geocoder answers `false`; that is the
/// honest answer here (the image has no geocoder provider and ACOS has no
/// network to reach one). Other location calls are named unsupported gaps —
/// they never occurred in the corpus before the frame rung.
#[derive(Debug, Default)]
pub struct LocationService;

impl LocationService {
    /// New handler.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ServiceObject for LocationService {
    fn call(&mut self, interface: &str, method: &MethodInfo, _args: &[Value]) -> Result<CallResult> {
        Err(unsupported(interface, method))
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        _data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != LOCATION_DESCRIPTOR {
            return Ok(false);
        }
        if code == LOCATION_TRANSACTION_GEOCODER_IS_PRESENT {
            // `geocoderIsPresent()` is `writeBoolean`, i.e. one int word.
            crate::exception::write_no_exception(reply);
            reply.write_int32(0);
            return Ok(true);
        }
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// ITrustManager ("trust")
// ---------------------------------------------------------------------------

/// Descriptor of `android.app.trust.ITrustManager`.
pub const TRUST_DESCRIPTOR: &str = "android.app.trust.ITrustManager";
const TRUST_REPORT_UNLOCK_ATTEMPT: u32 = 1;
const TRUST_REPORT_USER_REQUESTED_UNLOCK: u32 = 2;
const TRUST_REPORT_USER_MAY_REQUEST_UNLOCK: u32 = 3;
const TRUST_REPORT_UNLOCK_LOCKOUT: u32 = 4;
const TRUST_REPORT_ENABLED_AGENTS_CHANGED: u32 = 5;
const TRUST_REGISTER_LISTENER: u32 = 6;
const TRUST_UNREGISTER_LISTENER: u32 = 7;
const TRUST_REPORT_KEYGUARD_SHOWING_CHANGED: u32 = 8;
const TRUST_SET_DEVICE_LOCKED_FOR_USER: u32 = 9;
const TRUST_IS_DEVICE_LOCKED: u32 = 10;
const TRUST_IS_DEVICE_SECURE: u32 = 11;
const TRUST_IS_TRUST_USUALLY_MANAGED: u32 = 12;
const TRUST_UNLOCKED_BY_BIOMETRIC: u32 = 13;
const TRUST_CLEAR_ALL_BIOMETRIC_RECOGNIZED: u32 = 14;
const TRUST_IS_ACTIVE_UNLOCK_RUNNING: u32 = 15;

/// `android.app.trust.ITrustManager`: a device with no credential and no
/// keyguard.
///
/// `new KeyguardManager(context)` (API 34) resolves the `"trust"` service with
/// `ServiceManager.getServiceOrThrow` in its constructor; when the lookup
/// throws, `SystemServiceRegistry` turns the fetcher failure into a **null**
/// `getSystemService` result. `io.element.android.x`'s
/// `DefaultBiometricAuthenticatorManager.<init>` then calls
/// `keyguardManager.getClass()` and dies with an NPE — a *system* branch, since
/// a device always publishes `trust`.
///
/// Answers are the device's real state for a fresh single-user AOSP instance:
/// no device credential (`isDeviceSecure=false`), no keyguard shown
/// (`isDeviceLocked=false`), trust agents not managed, no active unlock. The
/// report/listener calls are accepted as no-ops exactly as `TrustManagerService`
/// accepts them from a client; they never change what an app can read here.
#[derive(Debug, Default)]
pub struct TrustService;

impl TrustService {
    /// New handler.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ServiceObject for TrustService {
    fn call(&mut self, interface: &str, method: &MethodInfo, _args: &[Value]) -> Result<CallResult> {
        Err(unsupported(interface, method))
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        _data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != TRUST_DESCRIPTOR {
            return Ok(false);
        }
        match code {
            TRUST_IS_DEVICE_LOCKED | TRUST_IS_DEVICE_SECURE
            | TRUST_IS_TRUST_USUALLY_MANAGED | TRUST_IS_ACTIVE_UNLOCK_RUNNING => {
                crate::exception::write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            TRUST_REPORT_UNLOCK_ATTEMPT
            | TRUST_REPORT_USER_REQUESTED_UNLOCK
            | TRUST_REPORT_USER_MAY_REQUEST_UNLOCK
            | TRUST_REPORT_UNLOCK_LOCKOUT
            | TRUST_REPORT_ENABLED_AGENTS_CHANGED
            | TRUST_REGISTER_LISTENER
            | TRUST_UNREGISTER_LISTENER
            | TRUST_REPORT_KEYGUARD_SHOWING_CHANGED
            | TRUST_SET_DEVICE_LOCKED_FOR_USER
            | TRUST_UNLOCKED_BY_BIOMETRIC
            | TRUST_CLEAR_ALL_BIOMETRIC_RECOGNIZED => {
                crate::exception::write_no_exception(reply);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_aidl::lookup_by_name;
    use acos_parcel::Parcel;

    fn method(interface: &str, name: &str) -> &'static MethodInfo {
        lookup_by_name(interface, name).expect("table entry")
    }

    fn call(
        service: &mut dyn ServiceObject,
        interface: &str,
        name: &str,
        args: &[Value],
    ) -> Vec<Value> {
        service
            .call(interface, method(interface, name), args)
            .expect("supported")
            .reply
    }

    /// R1: the reply is a non-null one-element array whose element round-trips
    /// through the same codec the framework's `StorageVolume(Parcel)` uses.
    #[test]
    fn storage_get_volume_list_is_one_mounted_emulated_volume() {
        let mut service = StorageService::new();
        let reply = call(
            &mut service,
            "android.os.storage.IStorageManager",
            "getVolumeList",
            &[Value::Int(0), Value::Str(Some("pkg".into())), Value::Int(0)],
        );
        let Value::List(Some(items)) = &reply[0] else {
            panic!("getVolumeList must answer an array, got {:?}", reply[0]);
        };
        let volume = items[0].as_parcelable().expect("a StorageVolume");
        assert_eq!(volume.class(), Some("android.os.storage.StorageVolume"));
        assert_eq!(volume.field(1).and_then(Value::as_str), Some("/storage/emulated/0"));
        assert_eq!(volume.field(13).and_then(Value::as_str), Some("mounted"));
        assert_eq!(volume.field(10).and_then(Value::as_parcelable).map(Parcelable::class), Some(Some("android.os.UserHandle")));

        // Write the one-element array exactly as `writeTypedArray` does, then
        // read it back through the codec: count, presence marker, payload.
        let mut parcel = Parcel::new();
        crate::codec::write_reply(
            &mut parcel,
            &method("android.os.storage.IStorageManager", "getVolumeList").reply,
            &reply,
            "android.os.storage.IStorageManager",
            "getVolumeList",
        )
        .expect("reply encodes");
        parcel.set_data_position(0);
        let n = parcel.read_int32().expect("count");
        assert_eq!(n, 1);
        assert_eq!(parcel.read_int32().expect("presence"), 1);
        let decoded = crate::parcelable::read_payload("android.os.storage.StorageVolume", &mut parcel)
            .expect("decodes");
        assert_eq!(decoded.class(), Some("android.os.storage.StorageVolume"));
    }

    #[test]
    fn shortcut_limits_and_job_schedule_are_the_platform_answers() {
        let mut shortcuts = ShortcutService::new(std::sync::Arc::new(std::sync::Mutex::new(
            ShortcutState::new(),
        )));
        assert_eq!(
            call(
                &mut shortcuts,
                "android.content.pm.IShortcutService",
                "getMaxShortcutCountPerActivity",
                &[Value::Str(Some("pkg".into())), Value::Int(0)],
            )[0],
            Value::Int(MAX_SHORTCUTS_PER_ACTIVITY)
        );
        let mut jobs = JobSchedulerService::new(std::sync::Arc::new(std::sync::Mutex::new(
            JobState::new(),
        )));
        let job = Parcelable::ordered("android.app.job.JobInfo", vec![Value::Int(42)]);
        assert_eq!(
            call(
                &mut jobs,
                "android.app.job.IJobScheduler",
                "schedule",
                &[
                    Value::Str(Some("pkg".into())),
                    Value::TypedObject(job),
                ],
            )[0],
            Value::Int(JOB_RESULT_SUCCESS)
        );
    }

    #[test]
    fn power_is_awake_and_accounts_are_empty() {
        let mut power = PowerService::new(std::sync::Arc::new(std::sync::Mutex::new(
            PowerState::new(),
        )));
        let reply = call(
            &mut power,
            "android.os.IPowerManager",
            "isInteractive",
            &[],
        );
        assert_eq!(reply, vec![Value::Bool(true)]);
        let mut accounts = AccountService::new();
        let reply = call(
            &mut accounts,
            "android.accounts.IAccountManager",
            "getAccountsAsUser",
            &[Value::Str(None), Value::Int(0), Value::Str(None)],
        );
        assert!(matches!(&reply[0], Value::List(Some(items)) if items.is_empty()));
    }
}
