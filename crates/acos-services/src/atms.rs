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

use crate::error::{Result, ServiceError};
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

    /// The framework parcelable one lifecycle item is written as.
    ///
    /// The item encodings live in [`crate::client_transaction`] (MPROC6): the
    /// host builds launch transactions and lifecycle transactions with the
    /// **same** item constructors, from the image's own `writeToParcel` order
    /// (this image's `StopActivityItem` has no `showWindow` on the wire, and its
    /// `PauseActivityItem` carries `mAutoEnteringPip`). `None` means ACOS has no
    /// decoded shape for the item on this path (a launch is built from the
    /// resolved intent + `ActivityInfo`, which only the launch encoder has), and
    /// nothing is written for it — the item is never invented.
    #[must_use]
    pub fn to_parcelable(&self) -> Option<Parcelable> {
        match self {
            // `PauseActivityItem(mFinished, mUserLeaving, mConfigChanges,
            // mDontReport, mAutoEnteringPip)`.
            Self::PauseActivity { finished } => Some(crate::client_transaction::pause_item(
                *finished, false, 0, false, false,
            )),
            // `StopActivityItem(configChanges)`.
            Self::StopActivity { .. } => Some(crate::client_transaction::stop_item(0)),
            // `ResumeActivityItem(procState, updateProcState, isForward,
            // shouldSendCompatFakeFocus)`; `PROCESS_STATE_TOP` is 2
            // (`ProcessList`), and the process state is genuinely re-evaluated.
            Self::ResumeActivity => Some(crate::client_transaction::resume_item(
                2, true, false, false,
            )),
            // `DestroyActivityItem(mFinished, mConfigChanges)`.
            Self::DestroyActivity { finished } => {
                Some(crate::client_transaction::destroy_item(*finished, 0))
            }
            Self::LaunchActivity { .. } | Self::RelaunchActivity { .. } => None,
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

    /// The **framework wire shape** of a one-item lifecycle transaction.
    ///
    /// `ClientTransaction.writeToParcel` (image dex) writes the activity-token
    /// presence flag + binder, the named lifecycle request, and the
    /// **callbacks-presence** flag. A lifecycle-only transaction has no
    /// callbacks, so the framework writes `false` and then no list at all
    /// (`mActivityCallbacks` stays null in `ClientTransaction.obtain`; MPROC6
    /// answered MPROC5's open question with the image's bytecode and the
    /// real-Java differential, and MPROC5b measured the failure it caused: the
    /// old encoding wrote a binder behind a `false` token marker and a `-1`
    /// list count behind a `false` callbacks marker, so the guest read a null
    /// token — `proc-a-paused-or-stopped-under-b` FAIL in run0/run1). The item
    /// is the transaction's lifecycle request, which is exactly what
    /// `TransactionExecutor` runs on the app's main thread. The encoding itself
    /// is the one [`crate::client_transaction`] encoder (MPROC6), shared with
    /// the launch path.
    ///
    /// `None` when the item has no decoded shape (nothing is fabricated).
    #[must_use]
    pub fn lifecycle_parcelable(item: &ClientTransactionItem, token: u64) -> Option<Parcelable> {
        let item = item.to_parcelable()?;
        Some(crate::client_transaction::client_transaction(
            Some(acos_parcel::BinderId(token)),
            Some(item),
            Vec::new(),
        ))
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
    /// MPROC7: the activity token that waits for this activity's result
    /// (`PendingLaunch.result_to`, i.e. the caller's activity token; `0` when
    /// nobody waits).
    pub result_to: u64,
    /// MPROC7: `startActivityForResult` request code (`-1`/`0` for a plain
    /// `startActivity`).
    pub request_code: i32,
    /// MPROC7: the result `finish(resultCode, data)` set (`RESULT_CANCELED` =
    /// `0` until the app sets one).
    pub result_code: i32,
    /// MPROC7: the result `Intent` (`finish`'s data; `None` when it passed
    /// `null`).
    pub result_data: Option<crate::ams::IntentFacts>,
    /// MPROC7: the result was taken from the record and queued to the caller's
    /// `IApplicationThread` (delivered once).
    pub result_delivered: bool,
    /// `IActivityClientController.onBackPressed` arrived for this token (ROT7).
    pub back_pressed: bool,
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
            result_to: 0,
            request_code: 0,
            result_code: 0,
            result_data: None,
            result_delivered: false,
            back_pressed: false,
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
    /// MPROC4 step 3b: the app process's tag (`0` = the launched/primary
    /// process). One record per tag; a tag is what names a process here.
    pub tag: u32,
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
    /// MPROC5c: when this process last held the foreground (its spawn time for a
    /// process that never resumed). The idle kill measures from here.
    pub last_active: std::time::Instant,
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
    /// Process records, one per app process tag (MPROC4 step 3b).
    pub processes: Vec<ProcessRecord>,
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

    /// MPROC5: the top-of-task activity token of each process (`tag -> token`).
    ///
    /// A device keeps this in the task record (`Task.topRunningActivity`); one
    /// record per process is enough here because ACOS schedules one task per
    /// process, and it is what the cross-process pause/stop/resume rule reads.
    pub tops: std::collections::BTreeMap<u32, u64>,
    /// MPROC5: lifecycle `ClientTransaction`s queued for a **specific**
    /// process's `IApplicationThread` (`tag`, method, args). `AtmsService::call`
    /// encodes them into `PendingDelivery`s; the peer routes each to the
    /// process that owns the target binder.
    pub pending_lifecycle_calls: Vec<(u32, String, Vec<Value>)>,
    /// MPROC5: the activity that was the session's top **before** the current
    /// one (`tag`, token). When the top finishes, stock resumes the activity
    /// below it in the task stack; ACOS keeps one task per process, so the
    /// record that just lost the top is the one below.
    pub previous_top: Option<(u32, u64)>,
    /// MPROC5: the session's foreground process tag, published to the launcher
    /// (`LiveHandles::top_process`) so the served probe tree/screenshot come
    /// from the process whose window is on top.
    pub top_process: Option<Arc<std::sync::atomic::AtomicU32>>,

    /// Pending `system -> app` calls to an arbitrary guest binder named by the
    /// transaction, as `(target binder, interface, method, args)` (ROT7).
    ///
    /// `IActivityClientController.onBackPressed` carries an
    /// `IRequestFinishCallback` the **server** is expected to call back; that
    /// callback binder is minted in the guest, so the reverse call goes through
    /// the same `CallResult::deliveries` path the `IApplicationThread`
    /// deliveries use (a `F_TXN` frame to the guest's Java binder).
    pub pending_reverse_calls: Vec<(u64, String, String, Vec<Value>)>,
    /// Granted permissions (install-time manifest grants; the runtime table in
    /// [`crate::permissions`] wins for a permission it has an answer for).
    pub granted_permissions: Vec<String>,
    /// The one runtime-permission table, shared with the PMS and the
    /// `permissions` module (MODNOTIF2). `None` in a bare unit-test state, in
    /// which case a granted permission answers from `granted_permissions`.
    pub runtime_permissions:
        Option<std::sync::Arc<std::sync::Mutex<crate::permissions::PermissionStore>>>,
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
            processes: Vec::new(),
            activities: Vec::new(),
            events: Vec::new(),
            protocol_events: Vec::new(),
            transactions: Vec::new(),
            pending_app_thread_calls: Vec::new(),
            tops: std::collections::BTreeMap::new(),
            previous_top: None,
            pending_lifecycle_calls: Vec::new(),
            top_process: None,
            pending_reverse_calls: Vec::new(),
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
        self.receiver_registrations
            .retain(|r| r.receiver != receiver);
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
        self.receiver_registrations
            .retain(|r| r.receiver != receiver);
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

    /// Record an attach and emit `process_started` (primary process).
    pub fn attach_application(&mut self, app_thread: u64, seq: i64, package: &str) {
        self.attach_application_for(0, app_thread, seq, package);
    }

    /// The record of process `tag`, if it has one.
    #[must_use]
    pub fn process(&self, tag: u32) -> Option<&ProcessRecord> {
        self.processes.iter().find(|p| p.tag == tag)
    }

    /// The mutable record of process `tag`.
    pub fn process_mut(&mut self, tag: u32) -> Option<&mut ProcessRecord> {
        self.processes.iter_mut().find(|p| p.tag == tag)
    }

    /// MPROC4 step 3b: a process exists because its guest connected with `tag`.
    ///
    /// `ActivityManagerService.startProcessLocked` creates the record when the
    /// zygote forks; the peer's handshake names the tag and the package, so this
    /// is the same event at the same point (before any attach). A record that
    /// already exists keeps its pid/uid (a re-attach is not a new process).
    pub fn process_started(&mut self, tag: u32, package: &str, uid: i32) {
        if self.process(tag).is_some() {
            return;
        }
        self.processes.push(ProcessRecord {
            tag,
            pid: 4242,
            uid,
            package_name: package.to_string(),
            app_thread: None,
            attach_seq: 0,
            attached: false,
            render_thread_tid: None,
            last_active: std::time::Instant::now(),
        });
        self.emit(LifecycleState::ProcessStarted, None);
    }

    /// MPROC5c: a process is gone (idle kill, crash, disconnect).
    ///
    /// Stock `ActivityManagerService.removeProcessLocked` drops the
    /// `ProcessRecord`; the tag's records (top-of-task, its activities) go with
    /// it, so the process list stops reporting it and a later `startActivity`
    /// for its package is a spawn again.
    pub fn process_died(&mut self, tag: u32) {
        let before = self.processes.len();
        self.processes.retain(|p| p.tag != tag);
        self.tops.remove(&tag);
        self.activities.retain(|a| process_tag_of(a.token) != tag);
        // MPROC7: the launcher serves the **foreground process's** probe sample.
        // When the process it published dies (idle kill, crash), that tag has no
        // samples left, so a tree reply would wait forever on a stale sample
        // (`probe-floor`, `tree_age_ms` growing). Stock moves the foreground to
        // the activity below; here that is `previous_top`, else any surviving
        // top, else the primary process.
        let published = self
            .top_process
            .as_ref()
            .map(|top| top.load(std::sync::atomic::Ordering::Relaxed));
        if published == Some(tag) {
            let next = self
                .previous_top
                .filter(|(other, _)| *other != tag && self.process(*other).is_some())
                .map(|(other, _)| other)
                .or_else(|| self.tops.keys().copied().find(|other| *other != tag))
                .unwrap_or(0);
            if let Some(top) = &self.top_process {
                top.store(next, std::sync::atomic::Ordering::Relaxed);
            }
        }
        if self.processes.len() != before {
            self.emit(LifecycleState::ProcessKilled, None);
        }
    }

    /// MPROC5c: mark `tag` as having just held the foreground.
    pub fn mark_process_active(&mut self, tag: u32) {
        if let Some(record) = self.processes.iter_mut().find(|p| p.tag == tag) {
            record.last_active = std::time::Instant::now();
        }
    }

    /// MPROC5c: whether `tag` has anything that keeps its process alive: a
    /// started/resumed/paused activity of its own, a started or bound service
    /// of its package, or (measured by the launcher) a visible window.
    #[must_use]
    pub fn process_has_work(&self, tag: u32) -> bool {
        let running = self.activities.iter().any(|a| {
            process_tag_of(a.token) == tag
                && !a.finished
                // Only a *started* activity keeps a process alive (stock: a
                // paused/stopped one is a cached process candidate).
                && matches!(a.lifecycle, ActivityLifecycle::Started | ActivityLifecycle::Resumed)
        });
        if running {
            return true;
        }
        let package = self
            .processes
            .iter()
            .find(|p| p.tag == tag)
            .map(|p| p.package_name.as_str())
            .unwrap_or_default();
        self.ams
            .services
            .values()
            .any(|s| s.package == package && (s.started || s.bind_count > 0))
    }

    /// MPROC5c: the processes the idle kill may take, with how long each has
    /// been idle.
    ///
    /// Only **lazily spawned** secondaries are candidates (`tag != 0`: the
    /// launched primary is the session's own process), and only while they hold
    /// nothing that needs a process — no started activity, no started/bound
    /// service. A process that has been idle for `idle` is returned.
    #[must_use]
    pub fn idle_processes(&self, idle: std::time::Duration) -> Vec<(u32, String, u64)> {
        let now = std::time::Instant::now();
        self.processes
            .iter()
            .filter(|p| p.tag != 0)
            .filter(|p| !self.process_has_work(p.tag))
            .filter_map(|p| {
                let idle_ms = now.duration_since(p.last_active).as_millis() as u64;
                (idle_ms >= idle.as_millis() as u64)
                    .then(|| (p.tag, p.package_name.clone(), idle_ms))
            })
            .collect()
    }

    /// MPROC5c: when the next idle process reaches `idle`, so the launcher's
    /// event loop can wake exactly then instead of polling.
    #[must_use]
    pub fn next_idle_deadline(&self, idle: std::time::Duration) -> Option<std::time::Instant> {
        self.processes
            .iter()
            .filter(|p| p.tag != 0)
            .filter(|p| !self.process_has_work(p.tag))
            .map(|p| p.last_active + idle)
            .min()
    }

    /// The process list a reply/verdict reports: one entry per process, in tag
    /// order (`tag`, package, uid, pid, `IApplicationThread` binder, attached).
    #[must_use]
    pub fn process_list(&self) -> Vec<(u32, String, i32, i32, Option<u64>, bool)> {
        let mut out: Vec<_> = self
            .processes
            .iter()
            .map(|p| {
                (
                    p.tag,
                    p.package_name.clone(),
                    p.uid,
                    p.pid,
                    p.app_thread,
                    p.attached,
                )
            })
            .collect();
        out.sort_by_key(|p| p.0);
        out
    }

    /// Record an attach for process `tag` and emit `process_started`.
    pub fn attach_application_for(&mut self, tag: u32, app_thread: u64, seq: i64, package: &str) {
        let package = if package.is_empty() {
            self.process(tag)
                .map(|p| p.package_name.clone())
                .unwrap_or_default()
        } else {
            package.to_string()
        };
        if let Some(process) = self.processes.iter_mut().find(|p| p.tag == tag) {
            process.app_thread = Some(app_thread);
            process.attach_seq = seq;
            if !package.is_empty() {
                process.package_name = package;
            }
            self.emit(LifecycleState::ProcessStarted, None);
            return;
        }
        self.processes.push(ProcessRecord {
            tag,
            pid: 4242,
            uid: crate::pms::FIXTURE_UID,
            package_name: package,
            app_thread: Some(app_thread),
            attach_seq: seq,
            attached: false,
            render_thread_tid: None,
            last_active: std::time::Instant::now(),
        });
        self.emit(LifecycleState::ProcessStarted, None);
    }

    /// `finishAttachApplication` (primary process).
    pub fn finish_attach_application(&mut self, seq: i64) -> Result<BindApplicationData> {
        self.finish_attach_application_for(0, seq)
    }

    /// `finishAttachApplication` for process `tag`.
    ///
    /// The primary process (tag 0) keeps the historical behaviour: the host
    /// synthesises the `bindApplication` + launch transaction it always did. A
    /// secondary process marshals its **own** `bindApplication` and
    /// `ClientTransaction` in the guest (the guest is this session's app side)
    /// and pushes them to the peer, so nothing is synthesised for it here — the
    /// peer delivers exactly what that process marshalled.
    pub fn finish_attach_application_for(
        &mut self,
        tag: u32,
        seq: i64,
    ) -> Result<BindApplicationData> {
        let data = {
            let Some(process) = self.processes.iter_mut().find(|p| p.tag == tag) else {
                return Err(ServiceError::IllegalState {
                    component: "atms",
                    detail: format!("finishAttachApplication for unknown process tag {tag}"),
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
        if tag != 0 {
            return Ok(data);
        }
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
        let (next, name) =
            {
                let activity = &mut self.activities[index];
                let next = activity.lifecycle.apply(transition).ok_or_else(|| {
                    ServiceError::IllegalState {
                        component: "atms",
                        detail: format!(
                            "illegal transition {:?} from {:?}",
                            transition, activity.lifecycle
                        ),
                    }
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
            atms_trace("adopt-rejected token=0x0 (null token)".into());
            return false;
        }
        if !self.processes.iter().any(|p| p.attached) {
            atms_trace(format!(
                "adopt-rejected token={token:#x} (process not attached)"
            ));
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
            None => {
                atms_trace(format!(
                    "adopt-rejected token={token:#x} (not guest-tagged, no recorded launch)"
                ));
                return false;
            }
        };
        if let Some(old) = placeholder {
            self.activities.retain(|a| a.token != old);
        }
        self.activities
            .retain(|a| a.token != FIXTURE_ACTIVITY_TOKEN);
        self.activities
            .push(ActivityRecord::new(token, name.clone()));
        self.log_event(
            "ActivityManagerAdopt",
            acos_protocol::LogLevel::Debug,
            format!("token={token:#x} activity={name}"),
        );
        // Also on the launcher's stderr: the adoption is the answer to W31's
        // `no activity for token 0x0`, so a live run must show it directly.
        eprintln!("acos-services: ActivityManagerAdopt token={token:#x} activity={name}");
        atms_trace(format!(
            "adopted token={token:#x} activity={name} placeholder={placeholder:?} guest_tagged={guest_binder}"
        ));
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
    ///
    /// The app reporting a resume is proof that `onCreate`/`onStart`/`onResume`
    /// already ran, so this is the writer that establishes the record's state
    /// when the host adopted the guest's own token (W38): an adopted record
    /// starts in `Uncreated`, and before MPROC5b the next honest report — the
    /// `activityStopped` a finishing activity sends — was rejected as
    /// `illegal transition Stop from Uncreated` (MPROC5b run0/run1).
    pub fn activity_resumed(&mut self, token: u64, resumed: bool) -> Result<()> {
        let index = self.ensure_known(token)?;
        if !resumed {
            self.activities[index].resumed = false;
            return Ok(());
        }
        match self.activities[index].lifecycle {
            ActivityLifecycle::Uncreated => {
                self.transition(token, Transition::Create)?;
                self.transition(token, Transition::Start)?;
            }
            ActivityLifecycle::Created => {
                self.transition(token, Transition::Start)?;
            }
            ActivityLifecycle::Resumed => {
                // A repeated report (the framework sends one per resume): the
                // record is already where it belongs.
                self.activities[index].resumed = true;
                self.top_of_task(token);
                return Ok(());
            }
            _ => {}
        }
        self.transition(token, Transition::Resume)?;
        self.top_of_task(token);
        Ok(())
    }

    /// Publish a process tag as the session's foreground process.
    fn publish_top(&mut self, tag: u32) {
        if let Some(top) = &self.top_process {
            top.store(tag, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// MPROC5: ATMS's **per-process** top-of-task rule.
    ///
    /// `token` just became the resumed activity of its process, so it is that
    /// process's top. On a device `ActivityTaskSupervisor` then sends the
    /// previous top's `PauseActivityItem` followed by its
    /// `StopActivityItem` to **that activity's own** `IApplicationThread`; here
    /// the same two framework items are queued for the process that lost the
    /// top (encoded from the image's `writeToParcel` shapes, delivered by the
    /// existing `PendingDelivery` transport). No state transition is applied
    /// locally: stock AMS also waits for `activityPaused`/`activityStopped`
    /// before the record moves, and the app's callbacks are the evidence.
    fn top_of_task(&mut self, token: u64) {
        let tag = self.activity_tag(token);
        let losers: Vec<(u32, u64)> = self
            .tops
            .iter()
            .filter(|(other, _)| **other != tag)
            .map(|(other, top)| (*other, *top))
            .collect();
        for (other_tag, other_token) in losers {
            let record = self
                .activities
                .iter()
                .find(|a| a.token == other_token)
                .cloned();
            let Some(record) = record else {
                self.tops.remove(&other_tag);
                continue;
            };
            if record.finished {
                self.tops.remove(&other_tag);
                continue;
            }
            // `resumed` is the flag the app's own `activityResumed` set; a
            // record can be `Started` in this model while the app has already
            // reported the resume, and stock ATMS pauses exactly the activity
            // that is resumed on screen.
            if record.resumed || matches!(record.lifecycle, ActivityLifecycle::Resumed) {
                self.queue_lifecycle(
                    other_tag,
                    other_token,
                    ClientTransactionItem::PauseActivity { finished: false },
                );
            }
            if matches!(
                record.lifecycle,
                ActivityLifecycle::Resumed | ActivityLifecycle::Started | ActivityLifecycle::Paused
            ) {
                self.queue_lifecycle(
                    other_tag,
                    other_token,
                    ClientTransactionItem::StopActivity { show_window: false },
                );
            }
            self.tops.remove(&other_tag);
            // The displaced top is the activity below the new one: the record a
            // finish of the new top returns to.
            self.previous_top = Some((other_tag, other_token));
        }
        self.tops.insert(tag, token);
        self.publish_top(tag);
        // MPROC5c: the foreground transition is what starts (and restarts) every
        // process's idle clock: the winner is active now, and the losers became
        // background now.
        self.mark_process_active(tag);
    }

    /// The process tag an activity token belongs to (peer binder id bits).
    fn activity_tag(&self, token: u64) -> u32 {
        self.activities
            .iter()
            .find(|a| a.token == token)
            .map(|_| process_tag_of(token))
            .unwrap_or_else(|| process_tag_of(token))
    }

    /// Queue one lifecycle item for process `tag`'s `IApplicationThread`.
    fn queue_lifecycle(&mut self, tag: u32, token: u64, item: ClientTransactionItem) {
        let Some(transaction) = ClientTransaction::lifecycle_parcelable(&item, token) else {
            self.log_event(
                "ActivityManagerLifecycle",
                acos_protocol::LogLevel::Warn,
                format!("tag={tag} token={token:#x} item={item:?} has no decoded wire shape"),
            );
            return;
        };
        self.pending_lifecycle_calls.push((
            tag,
            "scheduleTransaction".into(),
            vec![Value::TypedObject(transaction)],
        ));
        self.log_event(
            "ActivityManagerLifecycle",
            acos_protocol::LogLevel::Debug,
            format!("tag={tag} token={token:#x} item={item:?}"),
        );
    }

    /// MPROC5: the activity under a finished one resumes, across processes.
    ///
    /// Same-process resumes stay the app's own job (the framework's W38/ROT6
    /// watchers schedule `DestroyActivityItem` + `ResumeActivityItem`); when the
    /// activity below the finished one belongs to **another** process, the host
    /// is the only one that can reach it, so it queues one
    /// `ResumeActivityItem` for that process — the item stock ATMS sends.
    fn resume_below_after_finish(&mut self, finished: u64) {
        let finished_tag = self.activity_tag(finished);
        let next = self
            .ams
            .tasks
            .iter()
            .filter_map(|t| t.activities.last().copied())
            .find(|t| *t != finished)
            // MPROC5: the primary launched Activity is not created by a
            // `startActivity` the host recorded, so it is not in `ams.tasks`;
            // the process that lost the top (and was just paused) is the
            // activity below, exactly what stock resumes.
            .or_else(|| {
                self.previous_top
                    .filter(|(tag, _)| *tag != finished_tag)
                    .map(|(_, token)| token)
            });
        let Some(next) = next else { return };
        let next_tag = self.activity_tag(next);
        if next_tag == finished_tag {
            return;
        }
        let alive = self
            .activities
            .iter()
            .any(|a| a.token == next && !a.finished);
        if !alive {
            return;
        }
        self.queue_lifecycle(next_tag, next, ClientTransactionItem::ResumeActivity);
        self.tops.insert(next_tag, next);
        self.publish_top(next_tag);
        // MPROC5c: the finished process just went to the background, so its idle
        // clock starts now (the resumed one is active).
        self.mark_process_active(next_tag);
        self.mark_process_active(finished_tag);
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
    ///
    /// A stop for an activity that is already **destroyed** is a no-op, exactly
    /// as stock answers it: `ActivityTaskManagerService.activityStopped` looks
    /// the token up in the task manager's record map, which
    /// `onActivityDestroyed` has already cleared, so a late stop is ignored
    /// rather than an illegal transition. The guest produces that order itself:
    /// `ActivityThread.handleStopActivity` reports `activityStopped` from a
    /// posted `PendingTransactionActions.StopInfo` runnable, so a finishing
    /// activity's Stop report lands *after* its Destroy transaction's
    /// `activityDestroyed` (MPROC7 measured `illegal transition Stop from
    /// Destroyed` on proc-b's finish). AOSP's answer is the honest one; ACOS
    /// must not turn it into a structured error.
    pub fn activity_stopped(&mut self, token: u64, saved_clicks: Option<u32>) -> Result<()> {
        let index = self.ensure_known(token)?;
        if self.activities[index].lifecycle == ActivityLifecycle::Destroyed {
            self.log_event(
                "ActivityManagerStopped",
                acos_protocol::LogLevel::Debug,
                format!("token={token:#x} stop after destroy ignored (stock: record is gone)"),
            );
            return Ok(());
        }
        if let Some(clicks) = saved_clicks {
            self.save_instance_state(token, clicks);
        }
        self.transition(token, Transition::Stop).map(|_| ())
    }

    /// Report `activityDestroyed`.
    pub fn activity_destroyed(&mut self, token: u64) -> Result<()> {
        self.ensure_known(token)?;
        self.transition(token, Transition::Destroy)?;
        // MPROC7: the activity is gone; the result it set (if any) is final and
        // goes to the caller's own app thread.
        self.deliver_result(token);
        Ok(())
    }

    /// `onSaveInstanceState` bookkeeping.
    pub fn save_instance_state(&mut self, token: u64, clicks: u32) {
        if let Some(activity) = self.activities.iter_mut().find(|a| a.token == token) {
            activity.saved_clicks = clicks;
        }
        self.emit(LifecycleState::SaveInstanceState, None);
    }

    /// `IActivityClientController.onBackPressed(token, callback)` — the server
    /// half of the framework's own back path (API 34).
    ///
    /// `Activity.onBackPressed()` does **not** finish anything itself: it calls
    /// `ActivityClient.onBackPressed(mToken, new RequestFinishCallback(this))`
    /// (one-way, code 62), and `ActivityClientController.onBackPressed` is the
    /// side that decides. AOSP resolves the `ActivityRecord` for the token and
    /// asks the client to finish it through the callback it was handed; the
    /// callback's `requestFinish()` posts `Activity.finishAfterTransition()`,
    /// i.e. `Activity.finish()` → `IActivityClientController.finishActivity`,
    /// whose `true` is what flips `Activity.mFinished`. Everything after that
    /// (`onPause` → `onSaveInstanceState`/`onStop` → `onDestroy`) is a
    /// `ClientTransaction` the server sends.
    ///
    /// So this records the back press and queues the reverse call to the
    /// client's `IRequestFinishCallback`; the finish itself arrives
    /// independently as `finishActivity`, and is never synthesised here.
    ///
    /// Returns the callback binder that was queued, for the trace/test.
    pub fn on_back_pressed(&mut self, token: u64, callback: u64) -> Result<u64> {
        let index = self.ensure_known(token)?;
        atms_trace(format!(
            "back-pressed token={token:#x} callback={callback:#x} activity={}",
            self.activities[index].activity
        ));
        if callback == 0 {
            // AOSP's controller dereferences the callback; a null one is a
            // client bug and must not be turned into a silent finish.
            return Err(ServiceError::BadRequest {
                interface: "android.app.IActivityClientController".into(),
                method: "onBackPressed".into(),
                detail: format!("null IRequestFinishCallback for token {token:#x}"),
            });
        }
        self.activities[index].back_pressed = true;
        self.log_event(
            "ActivityBackPressed",
            acos_protocol::LogLevel::Debug,
            format!(
                "token={token:#x} activity={} action=request-finish",
                self.activities[index].activity
            ),
        );
        self.pending_reverse_calls.push((
            callback,
            "android.app.IRequestFinishCallback".into(),
            "requestFinish".into(),
            Vec::new(),
        ));
        Ok(callback)
    }

    /// `finishActivity` (the app asked to finish), with the result the app set.
    ///
    /// `IClientController.finishActivity(IBinder token, int resultCode, Intent
    /// resultData, boolean finishTask)` carries the `setResult` answer; stock
    /// AMS keeps it on the record and delivers it to the caller
    /// (`resultTo`) once the activity is gone. ACOS records the same fields and
    /// [delivers](Self::deliver_result) them at destroy.
    pub fn finish_activity(
        &mut self,
        token: u64,
        result_code: i32,
        result_data: Option<crate::ams::IntentFacts>,
    ) -> Result<()> {
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
                atms_trace(format!("finish-rejected token={token:#x} {e}"));
                return Err(e);
            }
        };
        self.activities[index].finished = true;
        self.activities[index].result_code = result_code;
        if result_data.is_some() {
            self.activities[index].result_data = result_data;
        }
        // Keep `AmsState`'s task stack in step: the finished Activity leaves the
        // task, so the record below it is the new top (what the guest's
        // destroy+resume transactions return to).
        self.ams.finish_activity(token);
        // MPROC7: the finished process's idle clock starts at the finish. Stock
        // refreshes it when the activity leaves the top; with a result delivery
        // the resume below is deferred to `deliver_result`, so this would
        // otherwise leave a stale `last_active` and make a just-finished process
        // an immediate idle-kill candidate.
        self.mark_process_active(self.activity_tag(token));
        // MPROC7: when the caller waits for a result, its resume travels with
        // that result (`ActivityResultItem` + `ResumeActivityItem`, stock ATMS's
        // shape), not as a second, earlier resume.
        let result_to = self.activities[index].result_to;
        let owes_result = result_to != 0 && result_to != token;
        if owes_result {
            self.log_event(
                "ActivityManagerResult",
                acos_protocol::LogLevel::Debug,
                format!(
                    "token={token:#x} finished code={result_code} -> result_to={result_to:#x} \
                     request={}",
                    self.activities[index].request_code
                ),
            );
        } else {
            // MPROC5: when the activity below belongs to another process, the
            // host resumes it (the guest can only resume its own process's
            // records).
            self.resume_below_after_finish(token);
        }
        Ok(())
    }

    /// MPROC7: deliver a finished activity's result to its caller.
    ///
    /// Stock `ActivityTaskSupervisor` sends the caller one `ClientTransaction`
    /// whose callback is `ActivityResultItem` (a `ResultInfo` with the who /
    /// requestCode / resultCode / data `Intent`) and whose lifecycle request is
    /// `ResumeActivityItem`: `onActivityResult` runs, then the caller resumes.
    /// This is the host-built transaction the ADR requires (no guest helper),
    /// encoded by the one MPROC6 encoder and handed to the caller's own
    /// `IApplicationThread` through the existing per-tag delivery transport.
    fn deliver_result(&mut self, token: u64) {
        let trace = std::env::var_os("ACOS_ATMS_TRACE").is_some();
        let Some(index) = self.activities.iter().position(|a| a.token == token) else {
            return;
        };
        let record = &self.activities[index];
        if trace {
            eprintln!(
                "ACOSATMS result token={token:#x} result_to={:#x} request={} delivered={} \
                 finished={}",
                record.result_to, record.request_code, record.result_delivered, record.finished
            );
        }
        if record.result_delivered || record.result_to == 0 || record.result_to == token {
            return;
        }
        let caller = record.result_to;
        let request_code = record.request_code;
        let result_code = record.result_code;
        let data = record.result_data.clone();
        let tag = self.activity_tag(caller);
        if !self.processes.iter().any(|p| p.tag == tag && p.attached) {
            self.log_event(
                "ActivityManagerResult",
                acos_protocol::LogLevel::Warn,
                format!(
                    "result for token={caller:#x} belongs to process {tag}, which is not attached"
                ),
            );
            return;
        }
        let data_intent = match data {
            Some(facts) => match crate::client_transaction::intent(&facts) {
                Ok(intent) => Some(intent),
                Err(err) => {
                    self.log_event(
                        "ActivityManagerResult",
                        acos_protocol::LogLevel::Warn,
                        format!("result data Intent could not be encoded: {err}"),
                    );
                    None
                }
            },
            None => None,
        };
        // `mResultWho` is **null** for a normal `startActivityForResult`
        // (`Instrumentation.execStartActivity` passes `mEmbeddedID`, which is
        // null; only an `ActivityGroup`-style embedded activity has a value).
        // `Activity.dispatchActivityResult` treats a non-null `who` as an
        // embedded id and does **not** call `onActivityResult` for it, so an
        // empty string silently swallowed the result (MPROC7).
        let info =
            crate::client_transaction::result_info(None, request_code, result_code, data_intent);
        let item = crate::client_transaction::activity_result_item(vec![info]);
        let resume = crate::client_transaction::resume_item(2, true, false, false);
        let transaction = crate::client_transaction::client_transaction(
            Some(acos_parcel::BinderId(caller)),
            Some(resume),
            vec![item],
        );
        self.log_event(
            "ActivityManagerResult",
            acos_protocol::LogLevel::Debug,
            format!(
                "tag={tag} token={caller:#x} ActivityResultItem(request={request_code}, \
                 code={result_code}) + ResumeActivityItem"
            ),
        );
        self.pending_lifecycle_calls.push((
            tag,
            "scheduleTransaction".into(),
            vec![Value::TypedObject(transaction)],
        ));
        self.activities[index].result_delivered = true;
        // The caller is the resumed, foreground activity now (stock: the result
        // transaction resumes it).
        self.tops.insert(tag, caller);
        self.publish_top(tag);
        self.mark_process_active(tag);
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

    /// ROT8b: does the host ATMS prove the task is finished?
    ///
    /// True only when no activity record is live (every record is finished or
    /// `Destroyed`), no `startActivity` is pending (AMS
    /// `pending_launches` is empty: a `finish()` + `startActivity(Home)` leaves
    /// Home's launch queued here until the guest materialises it), and no
    /// record is unnamed (a record with no class cannot be judged, so the
    /// answer fails closed). The guest's own process state alone is never
    /// enough: it cannot see a launch the host has accepted but not yet run.
    #[must_use]
    pub fn task_finished_proof(&self) -> bool {
        if !self.ams.pending_launches.is_empty() {
            return false;
        }
        self.activities
            .iter()
            .all(|a| {
                // `finished` (the framework's finishActivity) or a reported
                // destroy ends a record; anything else, including a record that
                // never left `Uncreated`, is treated as live (fail closed).
                !a.activity.is_empty()
                    && (a.finished || a.lifecycle == ActivityLifecycle::Destroyed)
            })
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
        let entry = state.provider_registry.acquire(method, authority, caller)?;
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

    /// MPROC4 step 3b: record that process `tag` exists (its guest connected).
    ///
    /// The uid is the uid already recorded for that package, else `0` (never a
    /// guessed uid).
    pub fn note_process_started(&mut self, tag: u32, package: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let uid = state
            .processes
            .iter()
            .find(|p| p.package_name == package)
            .map(|p| p.uid)
            .unwrap_or(0);
        state.process_started(tag, package, uid);
    }

    /// MPROC7: the launch recorded for `package` that the host still owes,
    /// as `(placeholder token, activity class)`.
    #[must_use]
    pub fn pending_launch_for(&self, package: &str) -> Option<(u64, String)> {
        let state = self.state.lock().ok()?;
        state
            .ams
            .pending_launch_for(package)
            .map(|launch| (launch.token, launch.class.clone()))
    }

    /// MPROC5c: record that process `tag` is gone (idle kill, crash, transport
    /// disconnect): its `ProcessRecord` and per-process records go with it.
    pub fn note_process_died(&mut self, tag: u32) {
        if let Ok(mut state) = self.state.lock() {
            state.process_died(tag);
        }
    }

    /// MPROC5c: the processes the idle kill may take
    /// (`tag`, package, idle milliseconds).
    #[must_use]
    pub fn idle_processes(&self, idle: std::time::Duration) -> Vec<(u32, String, u64)> {
        self.state
            .lock()
            .map(|state| state.idle_processes(idle))
            .unwrap_or_default()
    }

    /// MPROC5c: when the next idle process reaches `idle` (the launcher's wake).
    #[must_use]
    pub fn next_idle_deadline(&self, idle: std::time::Duration) -> Option<std::time::Instant> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.next_idle_deadline(idle))
    }

    /// MPROC4 step 3b: the processes this session has (one `ProcessRecord` per
    /// app process tag), as `(tag, package, uid, pid, app_thread, attached)`.
    #[must_use]
    pub fn process_list(&self) -> Vec<(u32, String, i32, i32, Option<u64>, bool)> {
        self.state
            .lock()
            .map(|state| state.process_list())
            .unwrap_or_default()
    }

    /// MPROC4 step 3b: drain the spawn requests the resolution paths recorded
    /// (`startActivity`/`startService`/`bindService`). The launcher owns the
    /// processes, so it is the one that turns a request into a guest.
    pub fn take_spawn_requests(&mut self) -> Vec<crate::ams::ProcessSpawnRequest> {
        self.state
            .lock()
            .map(|mut state| state.ams.take_spawn_requests())
            .unwrap_or_default()
    }

    /// The attached process record, if any: `(app thread binder id, attach
    /// seq, package)` (T-43 restore re-seed).
    #[must_use]
    pub fn attached_process(&self) -> Option<(u64, i64, String)> {
        self.state.lock().ok().and_then(|s| {
            s.process(0).map(|p| {
                (
                    p.app_thread.unwrap_or(0),
                    p.attach_seq,
                    p.package_name.clone(),
                )
            })
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
        self.seed_manifest_from_apk_with_uid(apk, crate::pms::FIXTURE_UID)
    }

    /// [`Self::seed_manifest_from_apk`] with the uid the package manager
    /// assigned. MPROC1: every installed package's manifest record must carry
    /// its **own** uid, because that uid decides which guest process an
    /// `android:process` declaration / component belongs to.
    pub fn seed_manifest_from_apk_with_uid(
        &mut self,
        apk: &std::path::Path,
        uid: i32,
    ) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.ams.manifests.add_apk(apk, uid)
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
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
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
        if std::env::var_os("ACOS_ATMS_TRACE").is_some() {
            let token = binder_u64(args.first());
            let known = state.activities.iter().any(|a| a.token == token);
            atms_trace(format!(
                "call {interface}.{} token={token:#x} known={known} known_tokens={:?} args={}",
                method.name,
                state.activities.iter().map(|a| a.token).collect::<Vec<_>>(),
                args.len()
            ));
        }
        let mut deliveries = Vec::new();
        let reply = match (self.interface, method.name.as_str()) {
            (AtmsInterface::ActivityManager, "checkPermission") => {
                let permission = args.first().and_then(Value::as_str).unwrap_or_default();
                vec![Value::Int(state.check_permission(permission))]
            }
            (AtmsInterface::ActivityManager, "attachApplication") => {
                let app_thread = args.first().and_then(Value::as_binder).map(|b| b.0);
                let seq = args.get(1).and_then(Value::as_long).unwrap_or(0);
                // MPROC4 step 3b: the `IApplicationThread` binder carries its
                // owning process tag in the peer's binder id, so the attach lands
                // on the right `ProcessRecord`. The package is the one the
                // process was started for (never a fabricated name).
                let tag = process_tag_of(app_thread.unwrap_or(0));
                state.attach_application_for(tag, app_thread.unwrap_or(0), seq, "");
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
                // The calling process is the one that just attached: the app
                // thread it named is recorded, and the last attach that is not
                // yet finished is that process (MPROC4 step 3b).
                let tag = state
                    .processes
                    .iter()
                    .filter(|p| !p.attached)
                    .max_by_key(|p| p.attach_seq)
                    .map(|p| p.tag)
                    .unwrap_or(0);
                let data = state.finish_attach_application_for(tag, seq)?;
                if tag == 0 {
                    state.queue_bind_application(&data);
                }
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
                let holder = self.content_provider_holder(
                    "getContentProvider",
                    authority,
                    caller,
                    &mut state,
                );
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
                let connection = args
                    .first()
                    .and_then(Value::as_binder)
                    .map(|b| b.0)
                    .unwrap_or(0);
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
                let connection = args
                    .first()
                    .and_then(Value::as_binder)
                    .map(|b| b.0)
                    .unwrap_or(0);
                let stable = args.get(1).and_then(Value::as_int).unwrap_or(0);
                let unstable = args.get(2).and_then(Value::as_int).unwrap_or(0);
                vec![Value::Bool(
                    state
                        .provider_registry
                        .ref_content_provider(connection, stable, unstable),
                )]
            }
            (AtmsInterface::ActivityManager, "setRenderThread") => {
                let tid = args.first().and_then(Value::as_int);
                // MPROC4: the render thread belongs to the calling process.
                let tag = process_tag_of(
                    args.first()
                        .and_then(Value::as_binder)
                        .map(|b| b.0)
                        .unwrap_or(0),
                );
                if let Some(process) = state.processes.iter_mut().find(|p| p.tag == tag) {
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
                // The AIDL boolean is `handleSplashScreenExit`, NOT a resumed
                // flag (image `IActivityClientController`
                // `oneway void activityResumed(IBinder token, boolean
                // handleSplashScreenExit)`). Reading it as "resumed" meant the
                // host recorded `resumed=false` on every real resume and never
                // ran the top-of-task rule (MPROC5b run0/run1: no
                // `activityPaused` reached proc-a). The callback itself is the
                // resume.
                state.activity_resumed(token, true)?;
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
            (AtmsInterface::ClientController, "onBackPressed") => {
                // ROT7: `onBackPressed(IBinder token,
                // IRequestFinishCallback callback)` (code 62, one-way). The
                // client has already decided that the framework's own back
                // handling ran; the server's half is to ask it to finish.
                let token = binder_u64(args.first());
                let callback = args.get(1).and_then(Value::as_binder).map(|b| b.0).unwrap_or(0);
                state.on_back_pressed(token, callback)?;
                Vec::new()
            }
            (AtmsInterface::ClientController, "finishActivity") => {
                // `finishActivity(IBinder token, int resultCode, Intent
                // resultData, int finishTask)`: the `setResult` answer travels
                // with the finish, exactly as stock AMS reads it.
                let token = binder_u64(args.first());
                let result_code = args.get(1).and_then(Value::as_int).unwrap_or(0);
                let result_data = args
                    .get(2)
                    .and_then(Value::as_parcelable)
                    .map(crate::ams::IntentFacts::from_parcelable);
                state.finish_activity(token, result_code, result_data)?;
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
        //
        // MPROC5: the per-process top-of-task items (pause/stop/resume) are
        // queued with the tag they belong to, because on a device every one of
        // them goes to **that** activity's own app thread, never to "the"
        // attached process.
        let lifecycle = std::mem::take(&mut state.pending_lifecycle_calls);
        for (tag, name, values) in lifecycle {
            let Some(app_thread) = state
                .processes
                .iter()
                .find(|p| p.tag == tag)
                .and_then(|p| p.app_thread)
            else {
                state.log_event(
                    "ActivityManagerLifecycle",
                    acos_protocol::LogLevel::Warn,
                    format!("tag={tag} has no IApplicationThread; {name} not delivered"),
                );
                continue;
            };
            let Some(info) = acos_aidl::lookup_by_name("android.app.IApplicationThread", &name)
            else {
                continue;
            };
            let Ok(data) =
                crate::codec::build_request("android.app.IApplicationThread", info, &values)
            else {
                state.log_event(
                    "ActivityManagerLifecycle",
                    acos_protocol::LogLevel::Warn,
                    format!("tag={tag} {name} could not be encoded"),
                );
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
        if let Some(app_thread) = state
            .processes
            .iter()
            .find(|p| p.attached)
            .and_then(|p| p.app_thread)
        {
            let pending = std::mem::take(&mut state.pending_app_thread_calls);
            for (name, values) in pending {
                if let Some(info) =
                    acos_aidl::lookup_by_name("android.app.IApplicationThread", &name)
                {
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
        // Encode any queued `system -> guest` reverse calls: the
        // `IApplicationThread` transaction deliveries above, and any call a
        // transaction named a guest binder for (`IRequestFinishCallback`).
        let reverse = std::mem::take(&mut state.pending_reverse_calls);
        for (target, interface, method, values) in reverse {
            let Some(info) = acos_aidl::lookup_by_name(&interface, &method) else {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface,
                    code: 0,
                    method,
                });
            };
            let data = crate::codec::build_request(&interface, info, &values)?;
            deliveries.push(PendingDelivery {
                target: acos_parcel::BinderId(target),
                interface,
                code: info.code,
                method,
                data,
            });
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

/// The app process tag a peer binder id names (MPROC4 step 3b).
///
/// `acos-binder-peer` writes a process tag into bits 34..40 of every binder id it
/// imports from a guest (`PROC_SHIFT`/`PROC_MASK`); a host binder has none of
/// those bits set, so it names the primary process (tag 0).
#[must_use]
pub fn process_tag_of(binder: u64) -> u32 {
    ((binder >> 34) & 0x3f) as u32
}

/// `ACOS_ATMS_TRACE` diagnostic sink.
///
/// `1` (or empty) writes the line to stderr; any other value is taken as a
/// file path and the line is appended there. The launcher's stderr is only
/// ring-buffered inside the CLI engine (8 KiB tail, surfaced on errors), so a
/// path is what a live-ART investigation needs: `ACOS_ATMS_TRACE=/path/log`.
fn atms_trace(line: String) {
    match std::env::var_os("ACOS_ATMS_TRACE") {
        None => {}
        Some(value) => {
            let value = value.to_string_lossy().into_owned();
            if value.is_empty() || value == "1" {
                eprintln!("ACOSATMS {line}");
                return;
            }
            use std::io::Write as _;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&value)
            {
                let _ = writeln!(file, "ACOSATMS {line}");
            }
        }
    }
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
    record.has_static_partial_types = filter.field(10).and_then(Value::as_int).unwrap_or(0) != 0;
    record.has_dynamic_partial_types = filter.field(11).and_then(Value::as_int).unwrap_or(0) != 0;
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
                kind: p
                    .and_then(|p| p.field(1))
                    .and_then(Value::as_int)
                    .unwrap_or(0),
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
                wild: p
                    .and_then(|p| p.field(2))
                    .and_then(Value::as_int)
                    .unwrap_or(0)
                    != 0,
                port: p
                    .and_then(|p| p.field(3))
                    .and_then(Value::as_int)
                    .unwrap_or(0),
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
