//! Mini `ActivityTaskManager` / `ActivityManager`: process attach and the
//! activity lifecycle transaction stream.
//!
//! The recorded order this crate is checked against is
//! `fixtures/reference/hello-views/art-api34-x86_64/lifecycle-summary.txt`:
//!
//! ```text
//! onCreate → onStart → onResume
//! onPause → onStop → onSaveInstanceState → onDestroy
//! onCreate → onStart → onResume
//! ...
//! ```
//!
//! ACOS emits a typed [`LifecycleEvent`] stream (also converted to
//! [`acos_protocol::Event`]) and a typed [`ClientTransaction`] stream. The
//! parcelable encoding of `ClientTransaction` is a documented V1 gap: the item
//! types are complete, the framework `ClientTransaction.writeToParcel` bytes are
//! not (it needs `LaunchActivityItem`/`ResumeActivityItem`/`TransactionExecutor`
//! parcelables from the VM). See `results/services/GAPS.md`.

use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;
use acos_protocol::{Event, LifecycleState};

use crate::error::{ServiceError, Result};
use crate::host::{CallResult, PendingDelivery, ServiceObject, Wiring};
use crate::parcelable;
use crate::value::{Parcelable, Value};

/// Activity lifecycle state (the mini-ATMS's authoritative model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityLifecycle {
    /// No activity record.
    Uncreated,
    /// `onCreate` ran.
    Created,
    /// `onStart` ran.
    Started,
    /// `onResume` ran.
    Resumed,
    /// `onPause` ran.
    Paused,
    /// `onStop` ran.
    Stopped,
    /// `onDestroy` ran.
    Destroyed,
}

impl ActivityLifecycle {
    /// Apply a transition, returning the new state or `None` when illegal.
    #[must_use]
    pub fn apply(self, transition: Transition) -> Option<Self> {
        use ActivityLifecycle::*;
        use Transition::*;
        match (self, transition) {
            (Uncreated, Create) | (Destroyed, Create) | (Destroyed, Relaunch) => Some(Created),
            (Created, Start) | (Paused, Start) => Some(Started),
            (Started, Resume) | (Paused, Resume) => Some(Resumed),
            (Resumed, Pause) => Some(Paused),
            (Paused, Stop) | (Started, Stop) => Some(Stopped),
            (Stopped, Destroy) | (Paused, Destroy) | (Resumed, Destroy) | (Created, Destroy) => {
                Some(Destroyed)
            }
            // A config-change relaunch tears the activity down and creates it
            // again with the saved state in one transaction.
            (Resumed, Relaunch) | (Paused, Relaunch) | (Stopped, Relaunch) => Some(Created),
            _ => None,
        }
    }

    /// The protocol state for the `onCreate`/`onStart` events a launch emits.
    #[must_use]
    pub fn is_alive(self) -> bool {
        !matches!(self, Self::Uncreated | Self::Destroyed)
    }
}

/// A lifecycle transition request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// `onCreate`.
    Create,
    /// `onStart`.
    Start,
    /// `onResume`.
    Resume,
    /// `onPause`.
    Pause,
    /// `onStop`.
    Stop,
    /// `onDestroy`.
    Destroy,
    /// Config-change relaunch.
    Relaunch,
}

/// One `ClientTransaction` item (the typed item stream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientTransactionItem {
    /// `LaunchActivityItem`.
    LaunchActivity {
        /// Activity class name.
        activity: String,
    },
    /// `ResumeActivityItem`.
    ResumeActivity,
    /// `PauseActivityItem`.
    PauseActivity {
        /// `finished`.
        finished: bool,
    },
    /// `StopActivityItem`.
    StopActivity {
        /// `showWindow`.
        show_window: bool,
    },
    /// `DestroyActivityItem`.
    DestroyActivity {
        /// `finished`.
        finished: bool,
    },
    /// `RelaunchActivityItem` (config change).
    RelaunchActivity {
        /// The saved instance state carried across the relaunch.
        saved_clicks: u32,
    },
}

impl ClientTransactionItem {
    /// Stable integer code used by the curated parcel.
    #[must_use]
    pub fn code(&self) -> i32 {
        match self {
            Self::LaunchActivity { .. } => 1,
            Self::ResumeActivity => 2,
            Self::PauseActivity { .. } => 3,
            Self::StopActivity { .. } => 4,
            Self::DestroyActivity { .. } => 5,
            Self::RelaunchActivity { .. } => 6,
        }
    }

    /// The lifecycle events this item executes in the app.
    #[must_use]
    pub fn events(&self) -> Vec<LifecycleState> {
        match self {
            Self::LaunchActivity { .. } => vec![LifecycleState::Created, LifecycleState::Started],
            Self::ResumeActivity => vec![LifecycleState::Resumed],
            Self::PauseActivity { .. } => vec![LifecycleState::Paused],
            Self::StopActivity { .. } => vec![LifecycleState::Stopped],
            Self::DestroyActivity { .. } => vec![LifecycleState::Destroyed],
            Self::RelaunchActivity { .. } => vec![LifecycleState::Recreated],
        }
    }
}

/// A typed `android.app.servertransaction.ClientTransaction`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClientTransaction {
    /// Items in execution order.
    pub items: Vec<ClientTransactionItem>,
}

impl ClientTransaction {
    /// Append an item.
    pub fn push(&mut self, item: ClientTransactionItem) {
        self.items.push(item);
    }

    /// Encode the curated `ClientTransaction` parcel (item codes only).
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        Parcelable::ordered(
            "android.app.servertransaction.ClientTransaction",
            vec![Value::IntArray(Some(
                self.items.iter().map(ClientTransactionItem::code).collect(),
            ))],
        )
    }
}

/// Typed `AppBindData` for `IApplicationThread.bindApplication`.
///
/// The field names follow `ActivityThread$AppBindData`; the parcel encoding is a
/// gap (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BindApplicationData {
    /// `processName`.
    pub process_name: String,
    /// `appInfo.packageName`.
    pub package_name: String,
    /// `appInfo.sourceDir`.
    pub source_dir: String,
    /// `appInfo.dataDir`.
    pub data_dir: String,
    /// `appInfo.nativeLibraryDir`.
    pub native_library_dir: String,
    /// `appInfo.uid`.
    pub uid: i32,
    /// `instrumentationName` (null on the launch path).
    pub instrumentation_name: Option<String>,
    /// `appInfo.className` (the `Application` class).
    pub app_class: String,
    /// `providers` (none for hello-views).
    pub providers: Vec<String>,
    /// `isRestrictedBackupMode`.
    pub restricted_backup_mode: bool,
    /// `persistent`.
    pub persistent: bool,
}


/// The synthetic activity token used by the in-process launch flow.
pub const FIXTURE_ACTIVITY_TOKEN: u64 = 0xabc;

/// One activity record.
#[derive(Debug, Clone)]
pub struct ActivityRecord {
    /// Window/activity token binder.
    pub token: u64,
    /// Activity class name.
    pub activity: String,
    /// Task id.
    pub task_id: i32,
    /// Display id.
    pub display_id: i32,
    /// Lifecycle state.
    pub lifecycle: ActivityLifecycle,
    /// `activityResumed` reported.
    pub resumed: bool,
    /// `activityIdle` reported (launch complete).
    pub idle: bool,
    /// `finish()` called.
    pub finished: bool,
    /// Saved instance state (`clicks`).
    pub saved_clicks: u32,
}

impl ActivityRecord {
    fn new(token: u64, activity: impl Into<String>) -> Self {
        Self {
            token,
            activity: activity.into(),
            task_id: 10,
            display_id: 0,
            lifecycle: ActivityLifecycle::Uncreated,
            resumed: false,
            idle: false,
            finished: false,
            saved_clicks: 0,
        }
    }
}

/// One typed lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleEvent {
    /// The protocol state.
    pub state: LifecycleState,
    /// Activity class name.
    pub activity: Option<String>,
}

/// The process record.
#[derive(Debug, Clone)]
pub struct ProcessRecord {
    /// Synthetic pid (the reference app pid is not stable across runs).
    pub pid: i32,
    /// uid.
    pub uid: i32,
    /// Package name.
    pub package_name: String,
    /// `IApplicationThread` binder.
    pub app_thread: Option<u64>,
    /// `attachApplication` seq.
    pub attach_seq: i64,
    /// `finishAttachApplication` seen.
    pub attached: bool,
    /// Render thread tid.
    pub render_thread_tid: Option<i32>,
}

/// The mini-ATMS state machine.
#[derive(Debug)]
pub struct AtmsState {
    /// Process record.
    pub process: Option<ProcessRecord>,
    /// Activity records (V1: one).
    pub activities: Vec<ActivityRecord>,
    /// Typed event stream.
    pub events: Vec<LifecycleEvent>,
    /// Protocol event stream (with seq/timestamp).
    pub protocol_events: Vec<Event>,
    /// Transactions issued to `IApplicationThread`.
    pub transactions: Vec<ClientTransaction>,
    /// Pending `system -> app` calls as `(method, args)`.
    pub pending_app_thread_calls: Vec<(String, Vec<Value>)>,
    /// Granted permissions.
    pub granted_permissions: Vec<String>,
    /// Logical clock (ms) advanced per event; the VM supplies the real clock.
    clock_ms: u64,
    /// Next event seq.
    next_seq: u64,
    /// Controller binder.
    pub controller_binder: u64,
    /// Content-provider binder.
    pub provider_binder: u64,
    /// Guest-initiated activity/service/broadcast/provider state (W50-A).
    pub ams: crate::ams::AmsState,
}

impl AtmsState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            process: None,
            activities: Vec::new(),
            events: Vec::new(),
            protocol_events: Vec::new(),
            transactions: Vec::new(),
            pending_app_thread_calls: Vec::new(),
            granted_permissions: vec!["android.permission.INTERNET".into()],
            clock_ms: 1,
            next_seq: 1,
            controller_binder: 0,
            provider_binder: 0,
            ams: crate::ams::AmsState::new(),
        }
    }

    fn emit(&mut self, state: LifecycleState, activity: Option<&str>) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let ts = self.clock_ms;
        self.clock_ms += 1;
        let activity = activity.map(str::to_string);
        self.events.push(LifecycleEvent {
            state,
            activity: activity.clone(),
        });
        self.protocol_events
            .push(Event::lifecycle(seq, ts, state, activity));
    }

    /// Record an attach and emit `process_started`.
    pub fn attach_application(&mut self, app_thread: u64, seq: i64, package: &str) {
        self.process = Some(ProcessRecord {
            pid: 4242,
            uid: crate::pms::FIXTURE_UID,
            package_name: package.to_string(),
            app_thread: Some(app_thread),
            attach_seq: seq,
            attached: false,
            render_thread_tid: None,
        });
        self.emit(LifecycleState::ProcessStarted, None);
    }

    /// `finishAttachApplication`: build and queue `bindApplication`, then the
    /// launch transaction.
    pub fn finish_attach_application(&mut self, seq: i64) -> Result<BindApplicationData> {
        let data = {
            let Some(process) = self.process.as_mut() else {
                return Err(ServiceError::IllegalState {
                    component: "atms",
                    detail: "finishAttachApplication before attachApplication".into(),
                });
            };
            if process.attach_seq != seq {
                return Err(ServiceError::IllegalState {
                    component: "atms",
                    detail: format!("attach seq {seq} != {}", process.attach_seq),
                });
            }
            process.attached = true;
            BindApplicationData {
                process_name: process.package_name.clone(),
                package_name: process.package_name.clone(),
                source_dir: String::new(),
                data_dir: String::new(),
                native_library_dir: String::new(),
                uid: process.uid,
                instrumentation_name: None,
                app_class: String::new(),
                providers: Vec::new(),
                restricted_backup_mode: false,
                persistent: false,
            }
        };
        self.emit(LifecycleState::AppCreated, None);
        // Real Android schedules the launch transaction after the bind. ACOS
        // synthesises the same two-item transaction so the first
        // `activityResumed`/`activityIdle` callbacks have a live record.
        self.ensure_activity(
            FIXTURE_ACTIVITY_TOKEN,
            "dev.acos.fixtures.helloviews.MainActivity",
        );
        let mut transaction = ClientTransaction::default();
        transaction.push(ClientTransactionItem::LaunchActivity {
            activity: "dev.acos.fixtures.helloviews.MainActivity".into(),
        });
        transaction.push(ClientTransactionItem::ResumeActivity);
        self.schedule(FIXTURE_ACTIVITY_TOKEN, transaction)?;
        Ok(data)
    }

    /// Queue a `bindApplication` delivery request (encoded by the native layer).
    pub fn queue_bind_application(&mut self, data: &BindApplicationData) {
        self.pending_app_thread_calls.push((
            "bindApplication".into(),
            vec![
                Value::Str(Some(data.process_name.clone())),
                Value::TypedObject(Parcelable::ordered(
                    "android.content.pm.ApplicationInfo",
                    vec![Value::Str8(Some(data.package_name.clone()))],
                )),
                Value::Str(Some(data.package_name.clone())),
                Value::Str(Some(data.source_dir.clone())),
                Value::TypedObject(Parcelable::Null), // ProviderInfoList
                Value::TypedObject(Parcelable::Null), // instrumentationName
                Value::TypedObject(Parcelable::Null), // ProfilerInfo
                Value::Bundle(None),
                Value::Binder(None),
                Value::Binder(None),
                Value::Int(0),
                Value::Bool(false),
                Value::Bool(data.restricted_backup_mode),
                Value::Bool(data.persistent),
                Value::Bool(false),
                Value::TypedObject(Parcelable::Null), // Configuration
                Value::TypedObject(Parcelable::Null), // CompatibilityInfo
                Value::Map(None),
                Value::Bundle(None),
                Value::Str(Some(data.app_class.clone())),
                Value::TypedObject(Parcelable::Null), // AutofillOptions
                Value::TypedObject(Parcelable::Null), // ContentCaptureOptions
                Value::LongArray(None),
                Value::TypedObject(Parcelable::Null), // SharedMemory
                Value::Long(0),
                Value::Long(0),
            ],
        ));
    }

    /// Ensure an activity record exists, creating it in `Uncreated`.
    pub fn ensure_activity(&mut self, token: u64, activity: &str) -> &mut ActivityRecord {
        if let Some(index) = self.activities.iter().position(|a| a.token == token) {
            &mut self.activities[index]
        } else {
            self.activities.push(ActivityRecord::new(token, activity));
            self.activities.last_mut().expect("just pushed")
        }
    }

    /// Apply a transition to the activity `token`, rejecting illegal moves.
    pub fn transition(&mut self, token: u64, transition: Transition) -> Result<ActivityLifecycle> {
        let Some(index) = self.activities.iter().position(|a| a.token == token) else {
            return Err(ServiceError::IllegalState {
                component: "atms",
                detail: format!("no activity for token {token:#x}"),
            });
        };
        let (next, name) = {
            let activity = &mut self.activities[index];
            let next = activity
                .lifecycle
                .apply(transition)
                .ok_or_else(|| ServiceError::IllegalState {
                    component: "atms",
                    detail: format!(
                        "illegal transition {:?} from {:?}",
                        transition, activity.lifecycle
                    ),
                })?;
            activity.lifecycle = next;
            (next, activity.activity.clone())
        };
        match transition {
            Transition::Create => self.emit(LifecycleState::Created, Some(&name)),
            Transition::Start => self.emit(LifecycleState::Started, Some(&name)),
            Transition::Resume => {
                if let Some(a) = self.activities.iter_mut().find(|a| a.token == token) {
                    a.resumed = true;
                }
                self.emit(LifecycleState::Resumed, Some(&name));
            }
            Transition::Pause => {
                if let Some(a) = self.activities.iter_mut().find(|a| a.token == token) {
                    a.resumed = false;
                }
                self.emit(LifecycleState::Paused, Some(&name));
            }
            Transition::Stop => {
                self.emit(LifecycleState::Stopped, Some(&name));
                self.emit(LifecycleState::SaveInstanceState, Some(&name));
            }
            Transition::Destroy => {
                if let Some(a) = self.activities.iter_mut().find(|a| a.token == token) {
                    a.finished = true;
                }
                self.emit(LifecycleState::Destroyed, Some(&name));
            }
            Transition::Relaunch => {
                self.emit(LifecycleState::Recreated, Some(&name));
            }
        }
        Ok(next)
    }

    /// Issue a `ClientTransaction` and emit the events it executes.
    pub fn schedule(&mut self, token: u64, transaction: ClientTransaction) -> Result<()> {
        if !self.activities.iter().any(|a| a.token == token) {
            return Err(ServiceError::IllegalState {
                component: "atms",
                detail: format!("no activity for token {token:#x}"),
            });
        }
        for item in &transaction.items {
            match item {
                ClientTransactionItem::LaunchActivity { .. } => {
                    self.transition(token, Transition::Create)?;
                    self.transition(token, Transition::Start)?;
                }
                ClientTransactionItem::ResumeActivity => {
                    self.transition(token, Transition::Resume)?;
                }
                ClientTransactionItem::PauseActivity { .. } => {
                    self.transition(token, Transition::Pause)?;
                }
                ClientTransactionItem::StopActivity { .. } => {
                    self.transition(token, Transition::Stop)?;
                }
                ClientTransactionItem::DestroyActivity { .. } => {
                    self.transition(token, Transition::Destroy)?;
                }
                ClientTransactionItem::RelaunchActivity { .. } => {
                    self.transition(token, Transition::Relaunch)?;
                }
            }
        }
        self.transactions.push(transaction);
        Ok(())
    }

    /// Report `activityResumed`.
    pub fn activity_resumed(&mut self, token: u64, resumed: bool) -> Result<()> {
        let Some(index) = self.activities.iter().position(|a| a.token == token) else {
            return Err(ServiceError::IllegalState {
                component: "atms",
                detail: format!("no activity for token {token:#x}"),
            });
        };
        self.activities[index].resumed = resumed;
        Ok(())
    }

    /// Report `activityIdle` (launch complete).
    pub fn activity_idle(&mut self, token: u64) -> Result<()> {
        let Some(index) = self.activities.iter().position(|a| a.token == token) else {
            return Err(ServiceError::IllegalState {
                component: "atms",
                detail: format!("no activity for token {token:#x}"),
            });
        };
        self.activities[index].idle = true;
        Ok(())
    }

    /// Report `activityPaused`.
    pub fn activity_paused(&mut self, token: u64) -> Result<()> {
        self.transition(token, Transition::Pause).map(|_| ())
    }

    /// Report `activityStopped` (with saved state).
    pub fn activity_stopped(&mut self, token: u64, saved_clicks: Option<u32>) -> Result<()> {
        if let Some(clicks) = saved_clicks {
            self.save_instance_state(token, clicks);
        }
        self.transition(token, Transition::Stop).map(|_| ())
    }

    /// Report `activityDestroyed`.
    pub fn activity_destroyed(&mut self, token: u64) -> Result<()> {
        self.transition(token, Transition::Destroy).map(|_| ())
    }

    /// `onSaveInstanceState` bookkeeping.
    pub fn save_instance_state(&mut self, token: u64, clicks: u32) {
        if let Some(activity) = self.activities.iter_mut().find(|a| a.token == token) {
            activity.saved_clicks = clicks;
        }
        self.emit(LifecycleState::SaveInstanceState, None);
    }

    /// `finishActivity` (the app asked to finish).
    pub fn finish_activity(&mut self, token: u64) -> Result<()> {
        let Some(index) = self.activities.iter().position(|a| a.token == token) else {
            return Err(ServiceError::IllegalState {
                component: "atms",
                detail: format!("no activity for token {token:#x}"),
            });
        };
        self.activities[index].finished = true;
        Ok(())
    }

    /// `checkPermission` from the manifest/grant policy.
    #[must_use]
    pub fn check_permission(&self, permission: &str) -> i32 {
        if self.granted_permissions.iter().any(|p| p == permission) {
            0
        } else {
            -1
        }
    }

    /// The lifecycle state of an activity.
    #[must_use]
    pub fn lifecycle_of(&self, token: u64) -> Option<ActivityLifecycle> {
        self.activities
            .iter()
            .find(|a| a.token == token)
            .map(|a| a.lifecycle)
    }
}

impl Default for AtmsState {
    fn default() -> Self {
        Self::new()
    }
}


/// Which interface a [`AtmsService`] is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtmsInterface {
    /// `android.app.IActivityManager`.
    ActivityManager,
    /// `android.app.IActivityTaskManager`.
    ActivityTaskManager,
    /// `android.app.IActivityClientController`.
    ClientController,
}

/// `IActivityManager` / `IActivityTaskManager` / `IActivityClientController`.
pub struct AtmsService {
    interface: AtmsInterface,
    state: Arc<Mutex<AtmsState>>,
    _wiring: Wiring,
}

impl AtmsService {
    /// Wrap shared ATMS state.
    #[must_use]
    pub fn new(interface: AtmsInterface, state: Arc<Mutex<AtmsState>>, wiring: Wiring) -> Self {
        if let Ok(mut state) = state.lock() {
            state.controller_binder = wiring.controller.0;
            state.provider_binder = wiring.content_provider.0;
        }
        Self {
            interface,
            state,
            _wiring: wiring,
        }
    }

    fn content_provider_holder(
        &self,
        authority: &str,
        state: &AtmsState,
    ) -> Option<Parcelable> {
        if authority != "settings" {
            return None;
        }
        let app =
            crate::pms::AppInfo::hello_views_fallback(std::path::Path::new("settings.apk"));
        let provider_info = crate::pms::ProviderInfo {
            name: "SettingsProvider".into(),
            package_name: "com.android.providers.settings".into(),
            authority: "settings".into(),
            process_name: "android.process.settings".into(),
            read_permission: "android.permission.READ_SETTINGS".into(),
            write_permission: "android.permission.WRITE_SETTINGS".into(),
            exported: false,
            grant_uri_permissions: false,
            init_order: 0,
        }
        .to_parcelable(&app);
        Some(Parcelable::ordered(
            "android.app.ContentProviderHolder",
            vec![
                Value::TypedObject(provider_info),                Value::TypedObject(Parcelable::ordered(
                    "android.os.IBinder",
                    vec![Value::Int(
                        i32::try_from(state.provider_binder).unwrap_or(-1),
                    )],
                )),
                Value::Bool(false),
            ],
        ))
    }
}

impl AtmsService {
    /// The attached process record, if any: `(app thread binder id, attach
    /// seq, package)` (T-43 restore re-seed).
    #[must_use]
    pub fn attached_process(&self) -> Option<(u64, i64, String)> {
        self.state.lock().ok().and_then(|s| {
            s.process
                .as_ref()
                .map(|p| (p.app_thread.unwrap_or(0), p.attach_seq, p.package_name.clone()))
        })
    }

    /// Re-create the `attachApplication` record after a snapshot restore.
    ///
    /// The snapshot carries the restored heap but not the in-process ATMS
    /// state, so without this `finishAttachApplication` reports
    /// `finishAttachApplication before attachApplication`. The record is the
    /// same one the boot's real `attach` produced.
    pub fn seed_attach(&mut self, app_thread: u64, seq: i64, package: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.attach_application(app_thread, seq, package);
        }
    }
}

impl ServiceObject for AtmsService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        args: &[Value],
    ) -> Result<CallResult> {
        let expected = match self.interface {
            AtmsInterface::ActivityManager => "android.app.IActivityManager",
            AtmsInterface::ActivityTaskManager => "android.app.IActivityTaskManager",
            AtmsInterface::ClientController => "android.app.IActivityClientController",
        };
        if interface != expected {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let mut state = self.state.lock().unwrap();
        let mut deliveries = Vec::new();
        let reply = match (self.interface, method.name.as_str()) {
            (AtmsInterface::ActivityManager, "checkPermission") => {
                let permission = args.first().and_then(Value::as_str).unwrap_or_default();
                vec![Value::Int(state.check_permission(permission))]
            }
            (AtmsInterface::ActivityManager, "attachApplication") => {
                let app_thread = args.first().and_then(Value::as_binder).map(|b| b.0);
                let seq = args.get(1).and_then(Value::as_long).unwrap_or(0);
                state.attach_application(
                    app_thread.unwrap_or(0),
                    seq,
                    "dev.acos.fixtures.helloviews",
                );
                vec![]
            }
            (AtmsInterface::ActivityManager, "finishAttachApplication") => {
                let seq = args.first().and_then(Value::as_long).unwrap_or(0);
                let data = state.finish_attach_application(seq)?;
                state.queue_bind_application(&data);
                vec![]
            }
            (AtmsInterface::ActivityManager, "getContentProvider") => {
                // The framework calls this as `(caller, context.getOpPackageName(),
                // name, userId, stable)`, so the requested authority is the
                // *second* string argument. Reading the first returned
                // `dev.acos.fixtures.counter` for the `settings` lookup,
                // `ContentResolver.acquireProvider` got `null`, and
                // `Settings$NameValueCache` NPE'd before `Activity.onCreate`.
                let authority = args.get(2).and_then(Value::as_str).unwrap_or_default();
                let holder = self.content_provider_holder(authority, &state);
                vec![match holder {
                    Some(h) => Value::TypedObject(h),
                    None => Value::TypedObject(Parcelable::Null),
                }]
            }
            (AtmsInterface::ActivityManager, "setRenderThread") => {
                let tid = args.first().and_then(Value::as_int);
                if let Some(process) = state.process.as_mut() {
                    process.render_thread_tid = tid;
                }
                vec![]
            }
            (AtmsInterface::ActivityManager, "registerStrictModeCallback")
            | (AtmsInterface::ActivityManager, "showWaitingForDebugger") => Vec::new(),
            (AtmsInterface::ActivityTaskManager, "getActivityClientController") => {
                vec![Value::Binder(Some(acos_parcel::BinderId(
                    state.controller_binder,
                )))]
            }
            (AtmsInterface::ActivityTaskManager, "reportAssistContextExtras") => Vec::new(),
            (AtmsInterface::ClientController, "getDisplayId") => vec![Value::Int(0)],
            (AtmsInterface::ClientController, "activityResumed") => {
                let token = binder_u64(args.first());
                let resumed = args.get(1).and_then(Value::as_bool).unwrap_or(true);
                state.activity_resumed(token, resumed)?;
                Vec::new()
            }
            (AtmsInterface::ClientController, "activityIdle") => {
                let token = binder_u64(args.first());
                state.activity_idle(token)?;
                Vec::new()
            }
            (AtmsInterface::ClientController, "activityPaused") => {
                let token = binder_u64(args.first());
                state.activity_paused(token)?;
                Vec::new()
            }
            (AtmsInterface::ClientController, "activityStopped") => {
                let token = binder_u64(args.first());
                let bundle = args.get(1).and_then(Value::as_bundle);
                let clicks = bundle
                    .and_then(|b| b.get("clicks"))
                    .and_then(Value::as_int)
                    .and_then(|v| u32::try_from(v).ok());
                state.activity_stopped(token, clicks)?;
                Vec::new()
            }
            (AtmsInterface::ClientController, "activityDestroyed") => {
                let token = binder_u64(args.first());
                state.activity_destroyed(token)?;
                Vec::new()
            }
            (AtmsInterface::ClientController, "setTaskDescription")
            | (AtmsInterface::ClientController, "reportSizeConfigurations") => Vec::new(),
            (AtmsInterface::ClientController, "finishActivity") => {
                let token = binder_u64(args.first());
                state.finish_activity(token)?;
                vec![Value::Bool(true)]
            }
            (AtmsInterface::ClientController, "activityLocalRelaunch") => {
                let token = binder_u64(args.first());
                let saved = state
                    .activities
                    .iter()
                    .find(|a| a.token == token)
                    .map(|a| a.saved_clicks)
                    .unwrap_or(0);
                let mut tx = ClientTransaction::default();
                tx.push(ClientTransactionItem::RelaunchActivity {
                    saved_clicks: saved,
                });
                tx.push(ClientTransactionItem::LaunchActivity {
                    activity: "dev.acos.fixtures.helloviews.MainActivity".into(),
                });
                tx.push(ClientTransactionItem::ResumeActivity);
                state.schedule(token, tx)?;
                Vec::new()
            }
            (AtmsInterface::ClientController, "activityRelaunched") => Vec::new(),
            _ => {
                if let Some(result) =
                    crate::ams::dispatch(&mut state.ams, interface, method, args)?
                {
                    return Ok(result);
                }
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                });
            }
        };

        // Encode any queued `IApplicationThread` transaction deliveries.
        if let Some(process) = state.process.as_ref() {
            if let Some(app_thread) = process.app_thread {
                let pending = std::mem::take(&mut state.pending_app_thread_calls);
                for (name, values) in pending {
                    if let Some(info) = acos_aidl::lookup_by_name("android.app.IApplicationThread", &name) {
                        let Ok(data) = crate::codec::build_request(
                            "android.app.IApplicationThread",
                            info,
                            &values,
                        ) else {
                            // The full `AppBindData` parcelables are a documented
                            // V1 gap; never fail the attach handshake because of it.
                            continue;
                        };
                        deliveries.push(PendingDelivery {
                            target: acos_parcel::BinderId(app_thread),
                            interface: "android.app.IApplicationThread".into(),
                            code: info.code,
                            method: name,
                            data,
                        });
                    }
                }
            }
        }
        let _ = parcelable::has_codec("android.app.servertransaction.ClientTransaction");
        Ok(CallResult { reply, deliveries })
    }

    fn drain_events(&mut self) -> Vec<Event> {
        let mut state = self.state.lock().unwrap();
        std::mem::take(&mut state.protocol_events)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn binder_u64(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_binder).map(|b| b.0).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// task / back-stack basics
// ---------------------------------------------------------------------------

/// The launch-mode decision for a `startActivity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchDecision {
    /// Deliver `onNewIntent` to the existing top instance (`singleTop`).
    ReuseTop,
    /// Clear the task above the existing instance and reuse it (`singleTask`).
    FinishAboveAndReuse,
    /// Create a new instance in the current task (`standard`).
    NewInstanceInTask,
    /// Create the activity in a new task (`singleInstance` / `NEW_TASK`).
    NewTask,
}

/// Decide how a launch is handled, from the manifest `launchMode` and whether
/// the target is already the top of its task.
///
/// `launch_mode` is the `ActivityInfo.launchMode` encoding:
/// `0=standard, 1=singleTop, 2=singleTask, 3=singleInstance`.
#[must_use]
pub fn decide_launch(launch_mode: i32, top_is_same: bool) -> LaunchDecision {
    match launch_mode {
        1 if top_is_same => LaunchDecision::ReuseTop,
        2 => LaunchDecision::FinishAboveAndReuse,
        3 => LaunchDecision::NewTask,
        _ => LaunchDecision::NewInstanceInTask,
    }
}

/// A minimal task/back-stack.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskStack {
    /// Monotonic task id.
    pub task_id: i32,
    /// Activity tokens from bottom to top.
    pub activities: Vec<u64>,
}

impl TaskStack {
    /// A new empty task.
    #[must_use]
    pub fn new(task_id: i32) -> Self {
        Self {
            task_id,
            activities: Vec::new(),
        }
    }

    /// Push a new activity on top.
    pub fn push(&mut self, token: u64) {
        self.activities.push(token);
    }

    /// Pop the top activity (the `back` key).
    pub fn pop(&mut self) -> Option<u64> {
        self.activities.pop()
    }

    /// The top activity token.
    #[must_use]
    pub fn top(&self) -> Option<u64> {
        self.activities.last().copied()
    }

    /// Remove everything above `token` (`singleTask` relaunch).
    pub fn clear_above(&mut self, token: u64) -> Vec<u64> {
        let Some(index) = self.activities.iter().position(|t| *t == token) else {
            return Vec::new();
        };
        self.activities.split_off(index + 1)
    }

    /// True when the task is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.activities.is_empty()
    }
}
