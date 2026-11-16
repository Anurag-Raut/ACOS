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

/// Bit 32 of a binder id: the peer's `JAVA_TAG` (`acos-binder-peer`), i.e. a
/// binder object owned by the guest JVM. Activity tokens minted by the guest's
/// `ActivityThread` arrive with this bit set; host-minted binder ids do not.
const GUEST_JAVA_BINDER_TAG: u64 = 1 << 32;

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

/// One `PatternMatcher` element of an `IntentFilter` (scheme-specific part or
/// data path), reduced from the decoded parcelable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PatternFacts {
    /// The pattern text (`mPattern`).
    pub pattern: Option<String>,
    /// `PatternMatcher.mType` (`PATTERN_LITERAL` = 0, `PATTERN_PREFIX` = 1,
    /// `PATTERN_SIMPLE_GLOB` = 2, `PATTERN_ADVANCED_GLOB` = 3).
    pub kind: i32,
}

/// One `IntentFilter$AuthorityEntry` element, reduced from the decoded
/// parcelable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthorityFacts {
    /// The host the entry was written for (`mOrigHost`).
    pub host: Option<String>,
    /// `mPort`, `-1` when the entry accepts any port.
    pub port: i32,
    /// `mWild` (`*.example.com`).
    pub wild: bool,
}

/// A dynamic receiver registration: `registerReceiver` /
/// `registerReceiverWithFeature` recorded exactly as the guest sent it.
///
/// The filter is reduced to its facts (the same order as
/// [`crate::parcelable`]'s `android.content.IntentFilter` spec) so the record
/// outlives the decoded parcel and can be inspected from tests and the event
/// stream. The receiver binder is kept as the numeric peer id, which is what
/// `unregisterReceiver` matches on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReceiverRegistration {
    /// The `IIntentReceiver` binder the guest registered.
    pub receiver: u64,
    /// `callerPackage` (arg 1 of both forms).
    pub package: String,
    /// `receiverId` (arg 3) on the WithFeature form; empty otherwise.
    pub receiver_id: String,
    /// `registerReceiver…` flags (arg 8 / arg 6): `RECEIVER_EXPORTED` etc.
    pub flags: i32,
    /// `IntentFilter.actions`.
    pub actions: Vec<String>,
    /// `IntentFilter.categories`.
    pub categories: Vec<String>,
    /// `IntentFilter.dataSchemes`.
    pub schemes: Vec<String>,
    /// `IntentFilter.mStaticDataTypes`.
    pub static_data_types: Vec<String>,
    /// `IntentFilter.mDataTypes`.
    pub data_types: Vec<String>,
    /// `IntentFilter.mMimeGroups`.
    pub mime_groups: Vec<String>,
    /// `IntentFilter.dataSchemeSpecificParts`.
    pub scheme_specific_parts: Vec<PatternFacts>,
    /// `IntentFilter.dataAuthorities`.
    pub authorities: Vec<AuthorityFacts>,
    /// `IntentFilter.dataPaths`.
    pub paths: Vec<PatternFacts>,
    /// `IntentFilter.priority`.
    pub priority: i32,
    /// `IntentFilter.hasStaticPartialTypes`.
    pub has_static_partial_types: bool,
    /// `IntentFilter.hasDynamicPartialTypes`.
    pub has_dynamic_partial_types: bool,
    /// `IntentFilter.autoVerify`.
    pub auto_verify: bool,
    /// `IntentFilter.mInstantAppVisibility`.
    pub instant_app_visibility: i32,
    /// `IntentFilter.mOrder`.
    pub order: i32,
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
    /// Granted permissions (install-time manifest grants; the runtime table in
    /// [`crate::permissions`] wins for a permission it has an answer for).
    pub granted_permissions: Vec<String>,
    /// The one runtime-permission table, shared with the PMS and the
    /// `permissions` module (MODNOTIF2). `None` in a bare unit-test state, in
    /// which case a granted permission answers from `granted_permissions`.
    pub runtime_permissions: Option<std::sync::Arc<std::sync::Mutex<crate::permissions::PermissionStore>>>,
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
    /// Dynamic receiver registrations (`registerReceiver[WithFeature]`), with
    /// the full decoded `IntentFilter` facts (W28).
    pub receiver_registrations: Vec<ReceiverRegistration>,
    /// The authorities backed by Rust providers (`settings`, …), with the
    /// reference counts `getContentProvider` maintains.
    pub provider_registry: crate::provider::ProviderRegistry,
    /// Registered `IIntentReceiver` callbacks as host-held strong references.
    /// `registerReceiver` takes one, `unregisterReceiver` drops it, and the
    /// module detach / session teardown releases whatever is left
    /// (`crate::refs`): without it the guest's receiver node would be
    /// unrooted the moment the app's own reference goes away, and
    /// `performReceive` would answer `DEAD_OBJECT`.
    pub refs: crate::refs::GuestRefs,
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
            runtime_permissions: None,
            clock_ms: 1,
            next_seq: 1,
            controller_binder: 0,
            provider_binder: 0,
            ams: crate::ams::AmsState::new(),
            receiver_registrations: Vec::new(),
            provider_registry: crate::provider::ProviderRegistry::new(),
            refs: crate::refs::GuestRefs::new(),
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

    /// Append a guest log line to the protocol event stream.
    ///
    /// Diagnostics that arrive over binder (a WTF report, a dropped
    /// permission, ...) are user-visible app behaviour on a phone because
    /// logcat records them. In ACOS the same information has to reach the
    /// guest log through the event stream, otherwise it is silently lost.
    pub fn log_event(&mut self, tag: &str, level: acos_protocol::LogLevel, message: String) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let ts = self.clock_ms;
        self.clock_ms += 1;
        self.protocol_events
            .push(Event::log(seq, ts, tag.to_string(), level, message));
    }

    /// Record a dynamic receiver registration.
    ///
    /// Stock `ActivityManagerService.registerReceiverWithFeature` looks the
    /// filter up against the sticky-broadcast store and answers the matching
    /// `Intent` (or `null`). ACOS keeps no sticky broadcasts, so the reply is
    /// `null` — the same thing a device with no matching sticky broadcast
    /// answers — and the registration is recorded here. Re-registering the same
    /// `IIntentReceiver` replaces the previous record, exactly like
    /// `AMS.unregisterReceiver` + register would.
    pub fn register_receiver(&mut self, registration: ReceiverRegistration) {
        let receiver = registration.receiver;
        let detail = describe_receiver(&registration);
        self.ams.register_receiver(
            receiver,
            &registration.package,
            registration.actions.clone(),
            registration.categories.clone(),
            registration.flags,
        );
        self.receiver_registrations.retain(|r| r.receiver != receiver);
        self.receiver_registrations.push(registration);
        if receiver != 0 {
            // A registration *is* a strong reference on the guest's
            // `IIntentReceiver` (a re-registration of the same binder is not a
            // second one).
            self.refs.hold(acos_parcel::BinderId(receiver));
        }
        self.log_event(
            "ActivityManagerReceiver",
            acos_protocol::LogLevel::Debug,
            detail,
        );
    }

    /// Drop a dynamic receiver registration (`unregisterReceiver`). Returns
    /// whether a record existed; stock throws `IllegalArgumentException` for an
    /// unknown receiver, ACOS answers the void call either way (the guest's own
    /// `LoadedApk` bookkeeping is the authority on what it registered).
    pub fn unregister_receiver(&mut self, receiver: u64) -> bool {
        let before = self.receiver_registrations.len();
        self.receiver_registrations.retain(|r| r.receiver != receiver);
        self.ams.unregister_receiver(receiver);
        if receiver != 0 {
            self.refs.release(acos_parcel::BinderId(receiver));
        }
        let removed = self.receiver_registrations.len() != before;
        if removed {
            self.log_event(
                "ActivityManagerReceiver",
                acos_protocol::LogLevel::Debug,
                format!("unregisterReceiver receiver={receiver:#x}"),
            );
        }
        removed
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

    /// Adopt a guest-minted activity token (W38).
    ///
    /// The guest's `ActivityThread` keys its `ActivityClientRecord` on the
    /// `IBinder` the launch `ClientTransaction` carried; that binder is created
    /// in the guest (`new Binder()`), so the host has never seen it. The first
    /// `IActivityClientController` callback for an unknown, non-zero token
    /// adopts it: the pending launch the host recorded for `startActivity` (if
    /// any) is bound to the real token, which replaces the host's placeholder
    /// token in `AmsState`'s task stack. The synthetic fixture record is dropped
    /// so the real activity is the only one this process owns.
    ///
    /// Returns whether the token is now known.
    fn adopt_activity(&mut self, token: u64) -> bool {
        if token == 0 {
            return false;
        }
        if !self.process.as_ref().is_some_and(|p| p.attached) {
            return false;
        }
        if self.activities.iter().any(|a| a.token == token) {
            return true;
        }
        // Only a token the peer tagged as a guest-owned Java binder
        // (`acos-binder-peer`'s `JAVA_TAG`, bit 32) can be an activity token the
        // guest minted. The other adoption route is a launch this host recorded
        // and the guest is now materialising. Anything else stays an error: an
        // arbitrary number must never become an activity.
        let guest_binder = token & GUEST_JAVA_BINDER_TAG != 0;
        let (name, placeholder) = match self.ams.adopt_launch_token(token) {
            Some((old, class)) => (class, Some(old)),
            None if guest_binder => ("activity".to_string(), None),
            None => return false,
        };
        if let Some(old) = placeholder {
            self.activities.retain(|a| a.token != old);
        }
        self.activities.retain(|a| a.token != FIXTURE_ACTIVITY_TOKEN);
        self.activities.push(ActivityRecord::new(token, name.clone()));
        self.log_event(
            "ActivityManagerAdopt",
            acos_protocol::LogLevel::Debug,
            format!("token={token:#x} activity={name}"),
        );
        // Also on the launcher's stderr: the adoption is the answer to W31's
        // `no activity for token 0x0`, so a live run must show it directly.
        eprintln!("acos-services: ActivityManagerAdopt token={token:#x} activity={name}");
        true
    }

    /// Index of the record for `token`, adopting a guest-minted token first.
    fn ensure_known(&mut self, token: u64) -> Result<usize> {
        if let Some(index) = self.activities.iter().position(|a| a.token == token) {
            return Ok(index);
        }
        if self.adopt_activity(token) {
            if let Some(index) = self.activities.iter().position(|a| a.token == token) {
                return Ok(index);
            }
        }
        Err(ServiceError::IllegalState {
            component: "atms",
            detail: format!("no activity for token {token:#x}"),
        })
    }

    /// Report `activityResumed`.
    pub fn activity_resumed(&mut self, token: u64, resumed: bool) -> Result<()> {
        let index = self.ensure_known(token)?;
        self.activities[index].resumed = resumed;
        Ok(())
    }

    /// Report `activityIdle` (launch complete).
    pub fn activity_idle(&mut self, token: u64) -> Result<()> {
        let index = self.ensure_known(token)?;
        self.activities[index].idle = true;
        Ok(())
    }

    /// Report `activityPaused`.
    pub fn activity_paused(&mut self, token: u64) -> Result<()> {
        self.ensure_known(token)?;
        self.transition(token, Transition::Pause).map(|_| ())
    }

    /// Report `activityStopped` (with saved state).
    pub fn activity_stopped(&mut self, token: u64, saved_clicks: Option<u32>) -> Result<()> {
        self.ensure_known(token)?;
        if let Some(clicks) = saved_clicks {
            self.save_instance_state(token, clicks);
        }
        self.transition(token, Transition::Stop).map(|_| ())
    }

    /// Report `activityDestroyed`.
    pub fn activity_destroyed(&mut self, token: u64) -> Result<()> {
        self.ensure_known(token)?;
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
        if std::env::var_os("ACOS_ATMS_TRACE").is_some() {
            eprintln!(
                "ACOSATMS finish-request token={token:#x} known={:?} n={}",
                self.activities.iter().map(|a| a.token).collect::<Vec<_>>(),
                self.activities.len()
            );
        }
        let index = match self.ensure_known(token) {
            Ok(index) => index,
            Err(e) => {
                if std::env::var_os("ACOS_ATMS_TRACE").is_some() {
                    eprintln!("ACOSATMS finish-rejected token={token:#x} {e}");
                }
                return Err(e);
            }
        };
        self.activities[index].finished = true;
        // Keep `AmsState`'s task stack in step: the finished Activity leaves the
        // task, so the record below it is the new top (what the guest's
        // destroy+resume transactions return to).
        self.ams.finish_activity(token);
        Ok(())
    }

    /// `checkPermission`: the shared runtime table first, then install-time.
    #[must_use]
    pub fn check_permission(&self, permission: &str) -> i32 {
        if let Some(store) = self.runtime_permissions.as_ref() {
            let store = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if store.entry(permission).is_some() {
                return store.check(permission);
            }
        }
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

    /// `IActivityManager.getContentProvider` / `getContentProviderExternal`.
    ///
    /// The registry of Rust providers ([`crate::provider::ProviderRegistry`])
    /// decides the answer; an unknown authority records a miss and returns
    /// `None`, which the caller turns into the `null` holder Android returns.
    /// The provider in the holder is the local `IContentProvider` binder
    /// `ServiceHost` dispatches to, and the connection is the real
    /// `ContentProviderConnection` token the framework writes back on release.
    fn content_provider_holder(
        &self,
        method: &str,
        authority: &str,
        caller: &str,
        state: &mut AtmsState,
    ) -> Option<Parcelable> {
        let entry = state
            .provider_registry
            .acquire(method, authority, caller)?;
        let app = crate::pms::AppInfo::hello_views_fallback(std::path::Path::new(&format!(
            "{}.apk",
            entry.package
        )));
        let provider_info = crate::pms::ProviderInfo {
            name: entry.class.clone(),
            package_name: entry.package.clone(),
            authority: entry.authority.clone(),
            process_name: entry.process_name.clone(),
            read_permission: entry.read_permission.clone(),
            write_permission: entry.write_permission.clone(),
            exported: entry.exported,
            grant_uri_permissions: entry.grant_uri_permissions,
            init_order: entry.init_order,
            meta_data: Vec::new(),
        }
        .to_parcelable(&app);
        // `ContentProviderHolder.writeToParcel` (API 34):
        // `info.writeToParcel` (no marker), provider `writeStrongBinder`,
        // connection `writeStrongBinder`, `noReleaseNeeded`, `mLocal`.
        Some(Parcelable::ordered(
            "android.app.ContentProviderHolder",
            vec![
                Value::Parcelable(provider_info),
                Value::Binder(Some(acos_parcel::BinderId(entry.binder))),
                Value::Binder(if entry.connection != 0 {
                    Some(acos_parcel::BinderId(entry.connection))
                } else {
                    None
                }),
                Value::Int(i32::from(entry.no_release_needed)),
                // The provider is served in this process, so `mLocal` is true.
                Value::Int(1),
            ],
        ))
    }
}

impl AtmsService {
    /// The ATMS state this service answers from (shared with every other user of
    /// the same wiring).
    ///
    /// The state now owns more than the lifecycle stream: the process/attach
    /// records and the [`crate::provider::ProviderRegistry`] that
    /// `getContentProvider` answers from. A decorator that wraps this service
    /// (the runtime's attach recorder) must reuse this `Arc` instead of a fresh
    /// [`AtmsState`], or the served provider set silently disappears.
    #[must_use]
    pub fn shared_state(&self) -> Arc<Mutex<AtmsState>> {
        Arc::clone(&self.state)
    }

    /// The structured record of every content-provider transaction this service
    /// has answered, oldest first — including `getContentProvider` misses (an
    /// unknown authority is a real answer with `hit: false`, not an error).
    #[must_use]
    pub fn provider_lookups(&mut self) -> Vec<crate::provider::ProviderLookup> {
        self.state
            .lock()
            .map(|state| state.provider_registry.lookups().to_vec())
            .unwrap_or_default()
    }

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

    /// Seed the component table `startActivity` resolves against (W38).
    ///
    /// The host's ATMS state starts empty, so a component launch decoded from
    /// `IActivityTaskManager.startActivity` resolved to `START_NOT_FOUND` even
    /// when the APK declared the target. The launcher calls this once, before
    /// the guest boots, with the same APK it installs in the PMS.
    pub fn seed_manifest_from_apk(&mut self, apk: &std::path::Path) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.ams.manifests.add_apk(apk, crate::pms::FIXTURE_UID)
    }

    /// The dynamic receiver registrations recorded so far, oldest first, with
    /// the decoded `IntentFilter` facts (W28).
    #[must_use]
    pub fn receiver_registrations(&mut self) -> Vec<ReceiverRegistration> {
        self.state
            .lock()
            .map(|state| state.receiver_registrations.clone())
            .unwrap_or_default()
    }

    /// Deliver a broadcast the **host** originates (a service module's control
    /// op: an incoming SMS, an adapter state change).
    ///
    /// The intent goes through the same `AmsState::broadcast` matching a guest
    /// broadcast uses, so only registered, matching receivers are reached. Every
    /// queued match becomes an `IIntentReceiver.performReceive` delivery (a
    /// manifest receiver is reported, not fabricated). Returns the match count
    /// and the deliveries the caller must enqueue.
    pub fn inject_broadcast(
        &mut self,
        intent: &crate::ams::IntentFacts,
    ) -> Result<(i32, Vec<crate::host::PendingDelivery>)> {
        let mut state = self.state.lock().unwrap();
        let matched = state.ams.broadcast(intent, 0, 0, None)?;
        let deliveries = take_receiver_deliveries(&mut state);
        Ok((matched, deliveries))
    }

    /// Record a dynamic receiver registration that the runtime's AMS already
    /// accepted (the session's one `registerReceiver`), so a broadcast the
    /// **system** originates is matched by the same receiver.
    pub fn register_receiver(
        &mut self,
        receiver: u64,
        package: &str,
        actions: Vec<String>,
        categories: Vec<String>,
        flags: i32,
    ) {
        let mut state = self.state.lock().unwrap();
        state.register_receiver(receiver_registration_from_parts(
            receiver, package, actions, categories, flags,
        ));
    }

    /// Drop a mirrored registration (the guest unregistered it).
    pub fn unregister_receiver(&mut self, receiver: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        state.unregister_receiver(receiver)
    }

    /// Drain the `IIntentReceiver.performReceive` deliveries the AMS state
    /// queued, whoever queued them.
    ///
    /// `PendingIntent` firing is the second producer: `IIntentSender.send`
    /// replays a `getBroadcast` sender through [`crate::ams::AmsState::broadcast`],
    /// which lands in the same queue a guest `broadcastIntent` uses. The peer's
    /// delivery drain calls this after every transaction and control op, so a
    /// fired sender reaches the app's dynamic receivers exactly like a
    /// host-originated broadcast ([`Self::inject_broadcast`]) does. A *manifest*
    /// receiver has no receiver binder in an ACOS session (there is no process
    /// to instantiate), so it is logged, never fabricated.
    pub fn take_receiver_deliveries(&mut self) -> Vec<crate::host::PendingDelivery> {
        let mut state = self.state.lock().unwrap();
        take_receiver_deliveries(&mut state)
    }
}

/// Convert the AMS state's queued receiver matches into `system -> app`
/// deliveries, logging the ones that cannot be delivered.
fn take_receiver_deliveries(state: &mut AtmsState) -> Vec<crate::host::PendingDelivery> {
    let queued = state.ams.take_pending_receivers();
    let mut deliveries = Vec::new();
    for receiver in queued {
        if receiver.is_manifest() {
            state.log_event(
                "ActivityManagerBroadcast",
                acos_protocol::LogLevel::Debug,
                format!(
                    "manifest receiver for {} not delivered: an ACOS session has no separate process to instantiate it",
                    receiver
                        .intent
                        .component
                        .as_ref()
                        .map(|(p, c)| format!("{p}/{c}"))
                        .unwrap_or_else(|| "(implicit)".to_string())
                ),
            );
            continue;
        }
        match receiver.receiver_delivery() {
            Ok(delivery) => deliveries.push(delivery),
            Err(error) => state.log_event(
                "ActivityManagerBroadcast",
                acos_protocol::LogLevel::Warn,
                format!("receiver delivery not encodable: {error}"),
            ),
        }
    }
    deliveries
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
            (AtmsInterface::ActivityManager, "handleApplicationWtf") => {
                // `handleApplicationWtf(IBinder app, String tag, boolean
                // system, ParcelableCrashInfo crashInfo, int immediateCallerPid)`.
                // Stock AMS shows the crash dialog and returns "should the
                // process be killed". ACOS has no dialog: it records the WTF as
                // a guest log line (so the first error of a crashing app is
                // visible in the ACOS log instead of vanishing) and answers
                // `false`, i.e. keep the process alive — the framework's own
                // default `ApplicationThread` handler
                // (`Thread.setDefaultUncaughtExceptionHandler` →
                // `RuntimeInit`/`LoggingHandler`) has already logged the
                // stack, and killing the guest here would lose the rest of the
                // launch for no fidelity gain.
                let tag = args
                    .get(1)
                    .and_then(Value::as_str)
                    .unwrap_or("WTF")
                    .to_string();
                let crash = args.get(3).and_then(Value::as_parcelable);
                let detail = describe_crash_info(crash);
                state.log_event(
                    "ActivityManagerWtf",
                    acos_protocol::LogLevel::Warn,
                    format!("{tag}: {detail}"),
                );
                vec![Value::Bool(false)]
            }
            (AtmsInterface::ActivityManager, "registerReceiver")
            | (AtmsInterface::ActivityManager, "registerReceiverWithFeature") => {
                // `registerReceiver(IApplicationThread caller, String
                // callerPackage, IIntentReceiver receiver, IntentFilter filter,
                // String permission, int userId, int flags)` (code 18) and
                // `registerReceiverWithFeature(caller, callerPackage,
                // callerFeatureId, receiverId, receiver, filter, permission,
                // userId, flags)` (code 19). Arg order read from the image's
                // `IActivityManager$Stub$Proxy` in `classes.dex`.
                //
                // ACOS keeps no sticky-broadcast store, so the answer is the
                // typed `null` `Intent` — what stock answers when no sticky
                // broadcast matches the filter. The receiver and every decoded
                // `IntentFilter` fact are recorded (W28); nothing is delivered
                // to the guest's `IIntentReceiver`, so no delivery is claimed.
                let with_feature = method.name == "registerReceiverWithFeature";
                let package = args.get(1).and_then(Value::as_str).unwrap_or_default();
                let (receiver_index, filter_index, flags_index, receiver_id) = if with_feature {
                    (
                        4,
                        5,
                        8,
                        args.get(3).and_then(Value::as_str).unwrap_or_default(),
                    )
                } else {
                    (2, 3, 6, "")
                };
                let receiver = args
                    .get(receiver_index)
                    .and_then(Value::as_binder)
                    .map(|b| b.0)
                    .unwrap_or(0);
                let filter = args.get(filter_index).and_then(Value::as_parcelable);
                let flags = args.get(flags_index).and_then(Value::as_int).unwrap_or(0);
                state.register_receiver(receiver_registration(
                    receiver,
                    package,
                    receiver_id.to_string(),
                    flags,
                    filter,
                ));
                vec![Value::TypedObject(Parcelable::Null)]
            }
            (AtmsInterface::ActivityManager, "unregisterReceiver") => {
                // `unregisterReceiver(IIntentReceiver receiver)`: void. Drop the
                // registration (and the `ams` matching record) so a later
                // broadcast cannot match a receiver that the guest removed.
                let receiver = binder_u64(args.first());
                state.unregister_receiver(receiver);
                Vec::new()
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
                let caller = args.get(1).and_then(Value::as_str).unwrap_or_default();
                let authority = args.get(2).and_then(Value::as_str).unwrap_or_default();
                let holder =
                    self.content_provider_holder("getContentProvider", authority, caller, &mut state);
                vec![match holder {
                    Some(h) => Value::TypedObject(h),
                    None => Value::TypedObject(Parcelable::Null),
                }]
            }
            (AtmsInterface::ActivityManager, "getContentProviderExternal") => {
                // `getContentProviderExternal(String name, int userId,
                // IBinder token, String callingPackage)`.
                let authority = args.first().and_then(Value::as_str).unwrap_or_default();
                let caller = args.get(3).and_then(Value::as_str).unwrap_or_default();
                let holder = self.content_provider_holder(
                    "getContentProviderExternal",
                    authority,
                    caller,
                    &mut state,
                );
                vec![match holder {
                    Some(h) => Value::TypedObject(h),
                    None => Value::TypedObject(Parcelable::Null),
                }]
            }
            (AtmsInterface::ActivityManager, "removeContentProvider") => {
                // One-way: `removeContentProvider(IBinder connection, boolean
                // stable)`. The connection is the token this host handed out in
                // the holder, so the registry resolves the authority from it.
                let connection = args.first().and_then(Value::as_binder).map(|b| b.0).unwrap_or(0);
                state
                    .provider_registry
                    .release("removeContentProvider", None, connection, "");
                Vec::new()
            }
            (AtmsInterface::ActivityManager, "removeContentProviderExternal") => {
                // `removeContentProviderExternal(String name, IBinder token)`.
                let authority = args.first().and_then(Value::as_str).unwrap_or_default();
                state.provider_registry.release(
                    "removeContentProviderExternal",
                    Some(authority),
                    0,
                    "",
                );
                Vec::new()
            }
            (AtmsInterface::ActivityManager, "refContentProvider") => {
                // `refContentProvider(IBinder connection, int stable, int
                // unstable)`: the framework asserts on a `false` reply.
                let connection = args.first().and_then(Value::as_binder).map(|b| b.0).unwrap_or(0);
                let stable = args.get(1).and_then(Value::as_int).unwrap_or(0);
                let unstable = args.get(2).and_then(Value::as_int).unwrap_or(0);
                vec![Value::Bool(state.provider_registry.ref_content_provider(
                    connection,
                    stable,
                    unstable,
                ))]
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
            // K2/R3: read-only activity queries. ACOS runs one activity in one
            // task; a token that names it has task id 1 (AOSP returns the task
            // id, or -1 when the token is unknown). The requested orientation
            // is whatever the manifest asked for, defaulting to
            // `SCREEN_ORIENTATION_UNSPECIFIED` (-1) like a real ActivityRecord.
            (AtmsInterface::ClientController, "getTaskForActivity") => {
                let token = binder_u64(args.first());
                let known = state.activities.iter().any(|a| a.token == token);
                vec![Value::Int(if known { 1 } else { -1 })]
            }
            (AtmsInterface::ClientController, "getRequestedOrientation") => {
                vec![Value::Int(-1)]
            }
            (AtmsInterface::ClientController, "willActivityBeVisible") => {
                vec![Value::Bool(true)]
            }
            _ => {
                let Some(result) = crate::ams::dispatch(&mut state.ams, interface, method, args)?
                else {
                    return Err(ServiceError::UnsupportedAndroidApi {
                        interface: interface.to_string(),
                        code: method.code,
                        method: method.name.clone(),
                    });
                };
                // `broadcastIntent` reaches the app's dynamic receivers: the
                // host materialises each `IIntentReceiver.performReceive` from
                // the queued match (ADR-0029 late modules need this for
                // `SMS_RECEIVED`). The conversion is shared with the
                // `IIntentSender` fire path ([`take_receiver_deliveries`]).
                deliveries.extend(take_receiver_deliveries(&mut state));
                return Ok(CallResult {
                    reply: result.reply,
                    deliveries,
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

    fn drain_retained(&mut self) -> Vec<acos_parcel::BinderId> {
        self.state.lock().unwrap().refs.drain_retained()
    }

    fn drain_released(&mut self) -> Vec<acos_parcel::BinderId> {
        self.state.lock().unwrap().refs.drain_released()
    }

    fn release_all_refs(&mut self) -> Vec<acos_parcel::BinderId> {
        self.state.lock().unwrap().refs.release_all()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn binder_u64(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_binder).map(|b| b.0).unwrap_or(0)
}

/// One-line description of a decoded `ParcelableCrashInfo` for the guest log.
///
/// Field indices follow `parcelable::SPECS` (which follows the image's
/// `CrashInfo.writeToParcel`): 0 handler class, 1 exception class, 2 message,
/// 3 throw file, 4 throw class, 5 throw method, 6 throw line, 7 stack trace,
/// 8 crash tag.
fn describe_crash_info(crash: Option<&Parcelable>) -> String {
    let Some(p) = crash else {
        return "null crash info".into();
    };
    let field = |i: usize| {
        p.field(i)
            .and_then(Value::as_str)
            .map(std::string::ToString::to_string)
    };
    let class = field(1).unwrap_or_else(|| "<no exception class>".into());
    let message = field(2).unwrap_or_default();
    let mut out = class;
    if !message.is_empty() {
        out.push_str(": ");
        out.push_str(&message);
    }
    if let Some(line) = p.field(6).and_then(Value::as_int) {
        if line > 0 {
            out.push_str(&format!(" (line {line})"));
        }
    }
    let stack = field(7).unwrap_or_default();
    let head: Vec<&str> = stack.lines().take(8).collect();
    if !head.is_empty() {
        out.push_str(" | ");
        out.push_str(&head.join(" | "));
    }
    out.chars().take(600).collect()
}

/// Reduce a decoded `android.content.IntentFilter` parcelable to the facts in
/// [`ReceiverRegistration`]. Field indices follow `parcelable::SPECS` (which
/// follows the image's `IntentFilter.writeToParcel`); a null filter is an empty
/// record, not an error.
/// A registration the runtime's AMS accepted, without a decoded `IntentFilter`
/// (the runtime keeps the decoded actions/categories it matched on).
fn receiver_registration_from_parts(
    receiver: u64,
    package: &str,
    actions: Vec<String>,
    categories: Vec<String>,
    flags: i32,
) -> ReceiverRegistration {
    ReceiverRegistration {
        receiver,
        package: package.to_string(),
        receiver_id: String::new(),
        flags,
        actions,
        categories,
        ..ReceiverRegistration::default()
    }
}

fn receiver_registration(
    receiver: u64,
    package: &str,
    receiver_id: String,
    flags: i32,
    filter: Option<&Parcelable>,
) -> ReceiverRegistration {
    let mut record = ReceiverRegistration {
        receiver,
        package: package.to_string(),
        receiver_id,
        flags,
        ..ReceiverRegistration::default()
    };
    let Some(filter) = filter else {
        return record;
    };
    record.actions = strings(filter.field(0));
    record.categories = cond_strings(filter.field(1));
    record.schemes = cond_strings(filter.field(2));
    record.static_data_types = cond_strings(filter.field(3));
    record.data_types = cond_strings(filter.field(4));
    record.mime_groups = cond_strings(filter.field(5));
    record.scheme_specific_parts = patterns(filter.field(6));
    record.authorities = authorities(filter.field(7));
    record.paths = patterns(filter.field(8));
    record.priority = filter.field(9).and_then(Value::as_int).unwrap_or(0);
    record.has_static_partial_types =
        filter.field(10).and_then(Value::as_int).unwrap_or(0) != 0;
    record.has_dynamic_partial_types =
        filter.field(11).and_then(Value::as_int).unwrap_or(0) != 0;
    record.auto_verify = filter.field(12).and_then(Value::as_int).unwrap_or(0) != 0;
    record.instant_app_visibility = filter.field(13).and_then(Value::as_int).unwrap_or(0);
    record.order = filter.field(14).and_then(Value::as_int).unwrap_or(0);
    // Field 15 is the optional `PersistableBundle`; ACOS records its presence
    // but does not model the bundle body here (it is not filter matching state).
    record
}

fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::StringArray(Some(items))) | Some(Value::StringList(Some(items))) => {
            items.iter().flatten().cloned().collect()
        }
        _ => Vec::new(),
    }
}

/// `Value::Cond { present, values: [StringList] }` -> its strings.
fn cond_strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Cond {
            present: true,
            values,
        }) => values.first().map_or_else(Vec::new, |v| strings(Some(v))),
        _ => Vec::new(),
    }
}

/// `Value::Cond { present, values: [List] }` -> its elements.
fn cond_list(value: Option<&Value>) -> Vec<&Value> {
    match value {
        Some(Value::Cond {
            present: true,
            values,
        }) => match values.first() {
            Some(Value::List(Some(items))) => items.iter().collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn patterns(value: Option<&Value>) -> Vec<PatternFacts> {
    cond_list(value)
        .into_iter()
        .map(|item| {
            let p = item.as_parcelable();
            PatternFacts {
                pattern: p
                    .and_then(|p| p.field(0))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                kind: p.and_then(|p| p.field(1)).and_then(Value::as_int).unwrap_or(0),
            }
        })
        .collect()
}

fn authorities(value: Option<&Value>) -> Vec<AuthorityFacts> {
    cond_list(value)
        .into_iter()
        .map(|item| {
            let p = item.as_parcelable();
            AuthorityFacts {
                host: p
                    .and_then(|p| p.field(0))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                wild: p.and_then(|p| p.field(2)).and_then(Value::as_int).unwrap_or(0) != 0,
                port: p.and_then(|p| p.field(3)).and_then(Value::as_int).unwrap_or(0),
            }
        })
        .collect()
}

/// One-line receiver/filter summary for the guest log (no secret material: an
/// `IntentFilter` carries only action/category/data matching rules).
fn describe_receiver(r: &ReceiverRegistration) -> String {
    let mut out = format!(
        "registerReceiver receiver={:#x} package={} flags={:#x} actions={:?} categories={:?}",
        r.receiver, r.package, r.flags, r.actions, r.categories
    );
    if !r.schemes.is_empty() {
        out.push_str(&format!(" schemes={:?}", r.schemes));
    }
    if !r.data_types.is_empty() {
        out.push_str(&format!(" types={:?}", r.data_types));
    }
    if !r.static_data_types.is_empty() {
        out.push_str(&format!(" static_types={:?}", r.static_data_types));
    }
    if !r.mime_groups.is_empty() {
        out.push_str(&format!(" mime_groups={:?}", r.mime_groups));
    }
    if !r.authorities.is_empty() {
        out.push_str(&format!(" authorities={:?}", r.authorities));
    }
    if !r.paths.is_empty() {
        out.push_str(&format!(" paths={:?}", r.paths));
    }
    if !r.scheme_specific_parts.is_empty() {
        out.push_str(&format!(" scheme_parts={:?}", r.scheme_specific_parts));
    }
    if r.priority != 0 {
        out.push_str(&format!(" priority={}", r.priority));
    }
    if r.auto_verify {
        out.push_str(" autoVerify");
    }
    if r.has_static_partial_types || r.has_dynamic_partial_types {
        out.push_str(&format!(
            " partial_types=({},{})",
            r.has_static_partial_types, r.has_dynamic_partial_types
        ));
    }
    out.chars().take(600).collect()
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
