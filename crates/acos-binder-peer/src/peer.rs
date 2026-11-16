//! The real `ServiceHost` behind the userspace binder driver.
//!
//! Responsibilities:
//!
//! * `servicemanager` (handle 0): `getService`/`checkService`/`listServices` go
//!   through [`ServiceHost::transact`] against the host's real service table;
//!   `addService` registers a Java-side local binder under a name.
//! * every other transaction: translate the driver's peer node id to the host's
//!   [`BinderId`], import the parcel (data + object table + fds), call
//!   [`ServiceHost::transact`], and export the reply parcel back.
//! * `system -> app` deliveries from [`ServiceHost::take_deliveries`] become
//!   `F_TXN` frames addressed at the Java local binder's driver handle.
//! * an unknown `(descriptor, code)` pair is logged as a structured
//!   `UNSUPPORTED_ANDROID_API` JSON line and answered with the host's exception
//!   reply. It is never turned into a success.
//!
//! # Binder id encoding across the driver
//!
//! `acos-parcel` ids are process-local. The driver hands the peer two different
//! kinds of reference, and they must stay distinguishable:
//!
//! | peer sees | meaning | encoded [`BinderId`] |
//! |---|---|---|
//! | `BINDER_TYPE_HANDLE(W)` | a Java/local binder the driver exported (wire handle `W`) | `JAVA_TAG \| W` |
//! | `BINDER_TYPE_BINDER(N)` | one of this peer's service nodes | `N` (`NODE_TAG \| host_id`) |
//!
//! On the way back `BINDER(N)` becomes `BINDER_TYPE_HANDLE` in Java (a
//! `BinderProxy`) and `JAVA_TAG|W` becomes `BINDER_TYPE_HANDLE(W)` again so the
//! framework gets the *same* Java `Binder` object.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use acos_parcel::binder::BINDER_TYPE_FD;
use acos_parcel::{BinderId, BinderKind, FlatObject, Parcel};
use acos_services::{codes, exception, registry, ServiceError, ServiceHost};

use crate::protocol::{
    Frame, F_HELLO, F_REP, F_REQ, F_RSP, F_TXN, INTERFACE_TRANSACTION, PING_TRANSACTION,
    TF_ONE_WAY, TF_STATUS_CODE,
};

/// Bit 32 marks a driver wire handle (a Java local binder).
pub const JAVA_TAG: u64 = 1 << 32;
/// Bit 33 marks one of this peer's service nodes.
pub const NODE_TAG: u64 = 1 << 33;
/// Bits 34..40 of a `JAVA_TAG` id name the app **process** that owns the driver
/// wire handle in the low 32 bits (MPROC2 step 3a).
///
/// A driver wire handle is only unique inside one guest process (each process
/// has its own driver with `next_handle = 1`), so `JAVA_TAG | wire` alone would
/// make two processes collide on the same [`BinderId`] — the second process's
/// `IApplicationThread` would then receive the first process's deliveries. The
/// tag is part of the id, so a delivery is routed to the connection that owns
/// it without a separate lookup and without an ambiguous id ever existing.
pub const PROC_SHIFT: u32 = 34;
/// The tag field width (`0x3f` = tags 0..=63).
pub const PROC_MASK: u64 = 0x3f;
/// The `acos.launch` peer node (P4a): the ART guest pushes the marshalled
/// `IApplicationThread.bindApplication` request here, and the peer delivers it
/// to the real `ApplicationThread` binder after `attachApplication`.
pub const LAUNCH_NODE: u64 = 1 << 40;
/// Bit 41 marks a **synthetic node for a binder that lives in another app
/// process** (MPROC3 step 3c).
///
/// A binder handle is only meaningful inside the process that owns it, so a
/// reply that carries another process's `BINDER_TYPE_HANDLE` would make the
/// receiver call a *different object*. The peer therefore hands the receiver a
/// node instead: `FOREIGN_FLAG | n` names `(owner_tag, owner_wire)`, and a
/// transaction addressed to it is forwarded to the owner's connection and its
/// reply relayed back. This is the routing ADR-0029 assigns to the Rust service
/// layer; the guest still sees one ordinary `IBinder` (`BinderProxy`), and its
/// `linkToDeath` works through [`crate::protocol::F_DEATH`].
pub const FOREIGN_FLAG: u64 = 1 << 41;
/// Foreign nodes one peer may allocate before it starts refusing (bounded, R1).
pub const MAX_FOREIGN_NODES: usize = 4096;

// --- bounded queues (R1 defence in depth) ---------------------------------
//
// Every structure below grows with what the *guest* does: a guest that retries a
// reverse delivery, registers binders, or asks for unsupported transactions in a
// loop would otherwise own an unbounded allocation in the host process. The
// ceilings are generous (a real launch is a few hundred transactions), and an
// overflow is a structured `ART_RESOURCE_LIMIT` outcome, never a silent drop.

/// Pending reverse deliveries held for the driver in one turn.
pub const MAX_PENDING_DELIVERIES: usize = 4096;
/// Recorded unsupported-transaction records (memory + the JSONL log).
pub const MAX_UNSUPPORTED_RECORDS: usize = 4096;
/// Recorded forwarded service transactions (the minimal-surface evidence).
pub const MAX_SERVICE_CALLS: usize = 8192;
/// Java binders the guest may register under one peer.
pub const MAX_JAVA_BINDERS: usize = 4096;
/// Framework-marshalled replies the guest may stash.
pub const MAX_LAUNCH_REPLIES: usize = 256;
/// `IServiceManager.getService` transaction codes seen on API 34 images.
/// (`getService`=1; the image build's `checkService`=2, which also returns the
/// binder for an existing name.)
const SM_GET_SERVICE: u32 = 1;
const SM_CHECK_SERVICE: u32 = 2;
/// `IApplicationThread.bindApplication` transaction code (API 34).
pub const BIND_APPLICATION_CODE: u32 = 5;
/// `acos.launch` transaction codes: push the marshalled bind request, then
/// framework-marshalled reply parcels for service calls whose ACOS codec is not
/// API-34-exact yet (see [`Peer::launch_reply_for`]).
pub const LAUNCH_PUSH_BIND: u32 = 1;
/// Push `(descriptor, code, reply bytes)`; the reply is produced by the
/// framework's own `writeToParcel` in the guest.
pub const LAUNCH_PUSH_REPLY: u32 = 2;
/// Push a marshalled `ClientTransaction` request, to be delivered to the app
/// thread as `IApplicationThread.scheduleTransaction` (P4b). The bytes are
/// written by the framework's own `ClientTransaction.writeToParcel`.
pub const LAUNCH_PUSH_TRANSACTION: u32 = 3;
/// `IApplicationThread.scheduleTransaction` transaction code (API 34).
pub const SCHEDULE_TRANSACTION_CODE: u32 = 54;

/// One transaction the peer forwarded during the launch, for the minimal
/// service-surface report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCall {
    /// AIDL descriptor from the request's interface token.
    pub descriptor: String,
    /// Transaction code.
    pub code: u32,
    /// AIDL method name (`code N` when the table has no entry).
    pub method: String,
}

const MASK32: u64 = 0xffff_ffff;

/// One `UNSUPPORTED_ANDROID_API` evidence record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedRecord {
    /// AIDL interface descriptor (`<unknown>` when the token was unreadable).
    pub descriptor: String,
    /// Transaction code.
    pub code: u32,
    /// Best-effort method name.
    pub method: String,
    /// Stable ACOS error code.
    pub error: String,
    /// Human-readable message.
    pub message: String,
}

impl UnsupportedRecord {
    fn to_json(&self) -> String {
        serde_json::json!({
            "kind": codes::UNSUPPORTED_ANDROID_API,
            "descriptor": self.descriptor,
            "code": self.code,
            "method": self.method,
            "error": self.error,
            "message": self.message,
        })
        .to_string()
    }
}

/// Per-connection launch state (the `acos.launch` pushes).
///
/// Every app process performs its own `attachApplication`, pushes its own
/// `bindApplication` bytes and its own `ClientTransaction`, so this state is
/// per process and never shared.
#[derive(Debug, Default)]
struct LaunchState {
    /// `IApplicationThread.bindApplication` bytes stashed by the guest.
    bind: Option<Vec<u8>>,
    /// The `IDisplayManager.getDisplayInfo` reply, marshalled by the
    /// framework's own `DisplayInfo.writeToParcel` in the guest.
    display_reply: Option<Vec<u8>>,
    /// Framework-marshalled reply parcels keyed by `(descriptor, code)`.
    replies: HashMap<(String, u32), Vec<u8>>,
    /// The `IApplicationThread` wire handle named by `attachApplication`.
    app_thread: Option<u32>,
    /// The launch delivery was queued (once).
    delivered: bool,
    /// The `IApplicationThread.scheduleTransaction` request bytes stashed by the
    /// guest (one `ClientTransaction`, framework-marshalled).
    txn: Option<Vec<u8>>,
    /// The transaction delivery was queued (once).
    txn_delivered: bool,
}

/// One app process's connection to the peer.
///
/// Step 3a: a session accepts **N** tagged connections (one per app process).
/// Everything a connection owns that used to live on [`Peer`] — the Java binder
/// table, the launch state — lives here, because a wire handle is only unique
/// inside its own process.
#[derive(Debug)]
pub struct ProcessConn {
    /// The process tag (`0` = the primary process).
    pub tag: u32,
    /// The process name from the handshake (evidence / diagnostics).
    pub name: String,
    /// Driver wire handle -> service name registered with `addService`.
    java_binders: HashMap<u32, String>,
    /// The connection is still open.
    connected: bool,
    launch: LaunchState,
}

impl ProcessConn {
    fn new(tag: u32, name: impl Into<String>) -> Self {
        Self {
            tag,
            name: name.into(),
            java_binders: HashMap::new(),
            connected: true,
            launch: LaunchState::default(),
        }
    }

    /// Driver wire handle -> service name registered with `addService`.
    #[must_use]
    pub fn java_binders(&self) -> &HashMap<u32, String> {
        &self.java_binders
    }

    /// True while the guest's connection is open.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.connected
    }
}

/// Why a connection handshake was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagRefusal {
    /// The tag is above [`crate::protocol::MAX_PROCESS_TAG`].
    OutOfRange(u32),
    /// Another live connection already claimed the tag.
    AlreadyConnected(u32),
    /// Every process slot is taken.
    TooManyProcesses(usize),
}

impl TagRefusal {
    /// Stable machine-readable reason.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::OutOfRange(_) => "PROCESS_TAG_OUT_OF_RANGE",
            Self::AlreadyConnected(_) => "PROCESS_TAG_IN_USE",
            Self::TooManyProcesses(_) => "PROCESS_TABLE_FULL",
        }
    }
}

impl std::fmt::Display for TagRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfRange(tag) => write!(
                f,
                "process tag {tag} is above the maximum {}",
                crate::protocol::MAX_PROCESS_TAG
            ),
            Self::AlreadyConnected(tag) => {
                write!(f, "process tag {tag} is already connected")
            }
            Self::TooManyProcesses(limit) => {
                write!(f, "the process table is full ({limit} processes)")
            }
        }
    }
}

/// Empty table returned for a process that never connected.
static EMPTY_BINDERS: std::sync::LazyLock<HashMap<u32, String>> =
    std::sync::LazyLock::new(HashMap::new);

/// One synthetic node the peer allocated for a binder owned by another process
/// (MPROC3 step 3c).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignNode {
    /// The process whose driver wire handle this node stands for.
    pub owner_tag: u32,
    /// That process's driver wire handle.
    pub wire: u32,
    /// Processes this node was written to. The node lives while it has holders;
    /// each holder is released when its connection closes (a per-handle release
    /// needs the driver's `BC_RELEASE` path, see `docs`/`STATUS` note).
    pub holders: Vec<u32>,
}

/// Name the primary connection carries until its own handshake arrives.
const DEFAULT_PRIMARY_NAME: &str = "primary";

/// The peer. One instance serves one session (N tagged driver connections).
pub struct Peer {
    host: ServiceHost,
    log_path: Option<PathBuf>,
    log: Option<File>,
    /// One entry per app process, indexed by tag.
    conns: Vec<Option<ProcessConn>>,
    /// Deliveries produced by the direct-dispatch path (the host owns the rest).
    local_deliveries: Vec<acos_services::PendingDelivery>,
    next_txn_seq: u32,
    /// Records written (also kept in memory for tests/evidence).
    pub unsupported: Vec<UnsupportedRecord>,
    /// Every request transaction the peer forwarded (the service surface).
    service_calls: Vec<ServiceCall>,
    /// Records/queues refused because a ceiling was reached (bounded queues).
    resource_limits: u64,
    /// Unsupported records dropped from the in-memory log once it was full.
    unsupported_dropped: u64,
    /// JSONL log lines written (`MAX_UNSUPPORTED_RECORDS` worth, at most).
    log_lines: usize,
    /// Reverse replies received from Java (peer-initiated transactions are
    /// two-way here, matching the runtime's `BinderDelivery`).
    pub reverse_replies: u64,
    /// How many of those replies were `TF_STATUS_CODE` failures, i.e. libbinder
    /// never dispatched the transaction to `onTransact` (typically
    /// `UNKNOWN_TRANSACTION` from a node that was not rooted). Counting these
    /// separately is what stops "20 replies" from being read as "20 deliveries".
    pub reverse_failed: u64,
    /// Synthetic nodes for binders owned by another process, by node id.
    foreign: HashMap<u64, ForeignNode>,
    /// `(owner_tag, wire)` -> synthetic node, so one binder has one node.
    foreign_index: HashMap<(u32, u32), u64>,
    /// Next synthetic node number (1-based; [`FOREIGN_FLAG`] is the tag bit).
    foreign_next: u64,
    /// Transactions the peer forwarded to an owner, keyed by
    /// `(owner_tag, forwarded seq)` -> `(caller tag, caller seq, node)`.
    pending_foreign: HashMap<(u32, u32), (u32, u32, u64)>,
    /// Death notifications waiting for the serve loop to write them.
    pending_deaths: Vec<(u32, Frame)>,
    /// Cross-process transactions forwarded to their owner and relayed back.
    pub foreign_forwards: u64,
    /// Cross-process transactions that found no live owner (structured refusals).
    pub foreign_dead: u64,
    /// Synthetic nodes freed because no holder process is left.
    pub foreign_released: u64,
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer")
            .field("host", &self.host)
            .field("processes", &self.tags())
            .field("java_binders", &self.java_binders())
            .field("unsupported", &self.unsupported.len())
            .field("reverse_replies", &self.reverse_replies)
            .field("reverse_failed", &self.reverse_failed)
            .finish()
    }
}

impl Peer {
    /// A peer with the ACOS standard service set and no unsupported log.
    #[must_use]
    pub fn new() -> Self {
        Self::with_host(ServiceHost::standard(), None)
    }

    /// A peer whose unsupported records are appended to `path` as JSON lines.
    #[must_use]
    pub fn with_log(path: impl AsRef<Path>) -> Self {
        Self::with_host(ServiceHost::standard(), Some(path.as_ref().to_path_buf()))
    }

    /// A peer around an existing host.
    #[must_use]
    pub fn with_host(host: ServiceHost, log_path: Option<PathBuf>) -> Self {
        let log = log_path.as_ref().and_then(|p| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        });
        let mut conns: Vec<Option<ProcessConn>> = (0..crate::protocol::MAX_PROCESSES)
            .map(|_| None)
            .collect();
        conns[crate::protocol::TAG_PRIMARY as usize] = Some(ProcessConn::new(
            crate::protocol::TAG_PRIMARY,
            DEFAULT_PRIMARY_NAME,
        ));
        Self {
            host,
            log_path,
            log,
            conns,
            local_deliveries: Vec::new(),
            next_txn_seq: 1,
            unsupported: Vec::new(),
            service_calls: Vec::new(),
            resource_limits: 0,
            unsupported_dropped: 0,
            log_lines: 0,
            reverse_replies: 0,
            reverse_failed: 0,
            foreign: HashMap::new(),
            foreign_index: HashMap::new(),
            foreign_next: 1,
            pending_foreign: HashMap::new(),
            pending_deaths: Vec::new(),
            foreign_forwards: 0,
            foreign_dead: 0,
            foreign_released: 0,
        }
    }

    /// The host (tests/inspection).
    #[must_use]
    pub fn host(&self) -> &ServiceHost {
        &self.host
    }

    /// The host, mutably (a `ServiceObject` the launcher has to configure).
    #[must_use]
    pub fn host_mut(&mut self) -> &mut ServiceHost {
        &mut self.host
    }

    /// Path of the JSON-lines unsupported log, when enabled.
    #[must_use]
    pub fn log_path(&self) -> Option<&Path> {
        self.log_path.as_deref()
    }

    /// The live process tags, ascending (tag 0 first when connected).
    #[must_use]
    pub fn tags(&self) -> Vec<u32> {
        self.conns
            .iter()
            .flatten()
            .filter(|c| c.connected)
            .map(|c| c.tag)
            .collect()
    }

    /// How many processes are connected.
    #[must_use]
    pub fn process_count(&self) -> usize {
        self.tags().len()
    }

    /// One process's connection state.
    #[must_use]
    pub fn process(&self, tag: u32) -> Option<&ProcessConn> {
        self.conns.get(tag as usize).and_then(Option::as_ref)
    }

    /// A process's name from the handshake.
    #[must_use]
    pub fn process_name(&self, tag: u32) -> Option<&str> {
        self.process(tag).map(|c| c.name.as_str())
    }

    /// Java binders registered through `addService` by the primary process
    /// (`wire handle -> name`).
    #[must_use]
    pub fn java_binders(&self) -> &HashMap<u32, String> {
        self.java_binders_for(crate::protocol::TAG_PRIMARY)
    }

    /// Java binders registered by process `tag` (`wire handle -> name`).
    #[must_use]
    pub fn java_binders_for(&self, tag: u32) -> &HashMap<u32, String> {
        self.process(tag).map_or(&EMPTY_BINDERS, |c| c.java_binders())
    }

    /// Every `(tag, wire, service name)` the session registered, ascending.
    #[must_use]
    pub fn all_java_binders(&self) -> Vec<(u32, u32, String)> {
        let mut v = Vec::new();
        for conn in self.conns.iter().flatten() {
            for (wire, name) in conn.java_binders() {
                v.push((conn.tag, *wire, name.clone()));
            }
        }
        v.sort();
        v
    }

    /// Transactions forwarded to the `ServiceHost` so far, in order (at most
    /// [`MAX_SERVICE_CALLS`]: the oldest are dropped and counted).
    #[must_use]
    pub fn service_calls(&self) -> &[ServiceCall] {
        &self.service_calls
    }

    /// How many requests were refused/evicted because a peer queue was full
    /// (`ART_RESOURCE_LIMIT`).
    #[must_use]
    pub fn resource_limits(&self) -> u64 {
        self.resource_limits
    }

    /// How many unsupported records fell out of the bounded evidence log.
    #[must_use]
    pub fn unsupported_dropped(&self) -> u64 {
        self.unsupported_dropped
    }

    /// The guest pushed the `bindApplication` request bytes.
    #[must_use]
    pub fn launch_bind_len(&self) -> Option<usize> {
        self.launch_state(crate::protocol::TAG_PRIMARY)?.bind.as_ref().map(Vec::len)
    }

    /// The `attachApplication` binder wire handle (primary process).
    #[must_use]
    pub fn launch_app_thread(&self) -> Option<u32> {
        self.launch_app_thread_for(crate::protocol::TAG_PRIMARY)
    }

    /// The `attachApplication` binder wire handle of process `tag`.
    #[must_use]
    pub fn launch_app_thread_for(&self, tag: u32) -> Option<u32> {
        self.launch_state(tag)?.app_thread
    }

    /// True when `bindApplication` was delivered to the app thread.
    #[must_use]
    pub fn launch_delivered(&self) -> bool {
        self.launch_delivered_for(crate::protocol::TAG_PRIMARY)
    }

    /// True when `bindApplication` was delivered to process `tag`.
    #[must_use]
    pub fn launch_delivered_for(&self, tag: u32) -> bool {
        self.launch_state(tag).is_some_and(|l| l.delivered)
    }

    /// The guest pushed a marshalled `ClientTransaction`.
    #[must_use]
    pub fn launch_txn_len(&self) -> Option<usize> {
        self.launch_state(crate::protocol::TAG_PRIMARY)?.txn.as_ref().map(Vec::len)
    }

    /// True when the `scheduleTransaction` delivery was queued.
    #[must_use]
    pub fn launch_txn_delivered(&self) -> bool {
        self.launch_state(crate::protocol::TAG_PRIMARY)
            .is_some_and(|l| l.txn_delivered)
    }

    /// The framework-marshalled `getDisplayInfo` reply, if the guest pushed one.
    #[must_use]
    pub fn launch_display_reply(&self) -> Option<&[u8]> {
        self.launch_state(crate::protocol::TAG_PRIMARY)?.display_reply.as_deref()
    }

    /// A framework-marshalled reply pushed by the guest for `(descriptor, code)`.
    #[must_use]
    pub fn launch_reply_for(&self, descriptor: &str, code: u32) -> Option<&[u8]> {
        self.launch_reply_for_tag(crate::protocol::TAG_PRIMARY, descriptor, code)
    }

    /// A framework-marshalled reply pushed by process `tag`'s guest.
    #[must_use]
    pub fn launch_reply_for_tag(&self, tag: u32, descriptor: &str, code: u32) -> Option<&[u8]> {
        self.launch_state(tag)?
            .replies
            .get(&(descriptor.to_string(), code))
            .map(Vec::as_slice)
    }

    fn launch_state(&self, tag: u32) -> Option<&LaunchState> {
        self.process(tag).map(|c| &c.launch)
    }

    fn launch_state_mut(&mut self, tag: u32) -> Option<&mut LaunchState> {
        self.conns
            .get_mut(tag as usize)
            .and_then(Option::as_mut)
            .map(|c| &mut c.launch)
    }

    /// Register a driver connection's process tag (the `F_HELLO` handshake).
    ///
    /// Tag 0 is the primary process and always exists; a hello for it only
    /// records the name. A duplicate, out-of-range or overflowing tag is a
    /// [`TagRefusal`] — the caller must refuse the connection, never silently
    /// fold it into another process.
    pub fn register_process(&mut self, tag: u32, name: &str) -> Result<(), TagRefusal> {
        if tag > crate::protocol::MAX_PROCESS_TAG {
            return Err(TagRefusal::OutOfRange(tag));
        }
        if self.conns.len() < crate::protocol::MAX_PROCESSES {
            self.conns
                .resize_with(crate::protocol::MAX_PROCESSES, || None);
        }
        let slot = &mut self.conns[tag as usize];
        match slot {
            Some(conn) if conn.connected => {
                if tag == crate::protocol::TAG_PRIMARY {
                    // The primary process re-announcing itself (a driver that
                    // reconnects after a restart) keeps its state and just
                    // updates the name.
                    if !name.is_empty() {
                        conn.name = name.to_string();
                    }
                    return Ok(());
                }
                Err(TagRefusal::AlreadyConnected(tag))
            }
            _ => {
                let name = if name.is_empty() {
                    format!("process-{tag}")
                } else {
                    name.to_string()
                };
                eprintln!("acos-binder-peer: process tag {tag} connected ({name})");
                *slot = Some(ProcessConn::new(tag, name.clone()));
                // MPROC4 step 3b: a process exists from the moment its guest
                // connects, exactly where `ActivityManagerService
                // .startProcessLocked` creates the record on a device. The
                // activity manager's `ProcessRecord` is per tag, so the attach
                // and the process list name the right package.
                self.record_process_started(tag, &name);
                Ok(())
            }
        }
    }

    /// MPROC4 step 3b: tell the activity manager that process `tag` exists.
    ///
    /// The peer owns the handshake (the first thing a guest does) and the
    /// activity manager owns the `ProcessRecord`; the package name comes from the
    /// guest's own `ACOS_PROCESS_NAME`, so nothing is invented here.
    fn record_process_started(&mut self, tag: u32, package: &str) {
        let Ok(object) = self.host.ensure("android.app.IActivityTaskManager") else {
            return;
        };
        if let Some(atms) = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<acos_services::atms::AtmsService>())
        {
            atms.note_process_started(tag, package);
        }
    }

    /// MPROC4 step 3b: drain the spawn requests the activity manager recorded
    /// while handling frames (`startActivity`, `startService`, `bindService` to a
    /// package with no process).
    pub fn take_spawn_requests(&mut self) -> Vec<acos_services::ams::ProcessSpawnRequest> {
        let Ok(object) = self.host.ensure("android.app.IActivityTaskManager") else {
            return Vec::new();
        };
        object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<acos_services::atms::AtmsService>())
            .map(acos_services::atms::AtmsService::take_spawn_requests)
            .unwrap_or_default()
    }

    /// MPROC4 step 3b: the activity manager's process list
    /// (`tag`, package, uid, pid, `IApplicationThread`, attached).
    pub fn process_list(&mut self) -> Vec<(u32, String, i32, i32, Option<u64>, bool)> {
        let Ok(object) = self.host.ensure("android.app.IActivityTaskManager") else {
            return Vec::new();
        };
        object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<acos_services::atms::AtmsService>())
            .map(|atms| atms.process_list())
            .unwrap_or_default()
    }

    /// Mark a process's connection closed and drop what it owned: its Java
    /// binder table, its launch state, and every binder the host recorded as
    /// owned by it (a delivery to a dead process must fail, not reach another
    /// process's handle).
    ///
    /// Returns `(jave binders dropped, host binder records dropped)`.
    pub fn disconnect_process(&mut self, tag: u32) -> (usize, usize) {
        let mut binders = 0;
        if let Some(slot) = self.conns.get_mut(tag as usize) {
            if let Some(conn) = slot.as_mut() {
                conn.connected = false;
                binders = conn.java_binders.len();
                conn.java_binders.clear();
                conn.launch = LaunchState::default();
                eprintln!(
                    "acos-binder-peer: process tag {tag} disconnected 
                     ({binders} java binders dropped)"
                );
            }
        }
        let owners = self.host.drop_process_binders(tag);
        // MPROC3 step 3c: the process's binders that other processes hold as
        // synthetic nodes are dead now. Release them and queue one
        // `F_DEATH` per holder (plus a structured refusal for every forwarded
        // transaction still waiting for an answer), so `linkToDeath` fires in
        // the holders instead of their next call hanging or reaching a
        // recycled handle.
        let released = self.release_foreign_of(tag);
        let freed_as_holder = self.release_foreign_holders_of(tag);
        if released > 0 {
            eprintln!(
                "acos-binder-peer: process tag {tag} disconnected ({released} foreign nodes \
                 released, {freed_as_holder} freed as holder, {} holders notified)",
                self.pending_deaths.len()
            );
        }
        (binders, owners)
    }

    /// Handle one incoming frame and return the frames to send back.
    ///
    /// This is the primary process's connection (tag 0): the one-guest fast
    /// path every pre-MPROC2 caller and test uses.
    pub fn handle(&mut self, frame: Frame) -> Vec<Frame> {
        self.handle_tagged(crate::protocol::TAG_PRIMARY, frame)
            .into_iter()
            .map(|(_, f)| f)
            .collect()
    }

    /// Handle one frame from process `tag`'s connection.
    ///
    /// The returned frames carry the tag of the connection they must be written
    /// to: a reply goes back on the caller's connection, a reverse delivery to
    /// the process that owns the target binder.
    pub fn handle_tagged(&mut self, tag: u32, frame: Frame) -> Vec<(u32, Frame)> {
        let mut out: Vec<(u32, Frame)> = Vec::new();
        match frame.ty {
            F_HELLO => {
                // A handshake on an established connection never carries a
                // transaction; it either confirms the tag the launcher already
                // registered (and updates the name) or reports a mismatch.
                let (hello_tag, name) = frame.hello_of().unwrap_or((tag, String::new()));
                if hello_tag != tag {
                    self.log_unsupported(&UnsupportedRecord {
                        descriptor: "<peer>".into(),
                        code: 0,
                        method: "hello".into(),
                        error: codes::UNSUPPORTED_ANDROID_API.into(),
                        message: format!(
                            "handshake for tag {hello_tag} on the connection of tag {tag}"
                        ),
                    });
                } else if !name.is_empty() {
                    if let Some(conn) = self
                        .conns
                        .get_mut(tag as usize)
                        .and_then(Option::as_mut)
                    {
                        conn.name = name;
                    }
                }
            }
            F_REQ => {
                // A transaction addressed to a synthetic foreign node belongs to
                // another process (MPROC3 step 3c): forward it to the owner and
                // relay the owner's reply back. It has no immediate reply, so it
                // is never written to this connection.
                if frame.target & FOREIGN_FLAG != 0 {
                    let (dest, forwarded) = self.forward_foreign(tag, frame.target, &frame);
                    out.push((dest, forwarded));
                } else if let Some(reply) = self.handle_request(tag, &frame) {
                    if frame.flags & TF_ONE_WAY == 0 {
                        out.push((tag, reply));
                    }
                }
            }
            F_REP => {
                self.reverse_replies += 1;
                // A reply to a transaction this peer forwarded from another
                // process goes back to that process, with its own seq.
                if let Some(relay) = self.relay_foreign_reply(tag, &frame) {
                    out.push(relay);
                } else if frame.flags & TF_STATUS_CODE != 0 {
                    self.reverse_failed += 1;
                    eprintln!(
                        "acos-binder-peer: process {tag}: reverse delivery seq={} was NOT \
                         dispatched (TF_STATUS_CODE reply: libbinder never reached onTransact)",
                        frame.seq
                    );
                } else if !frame.data.is_empty() {
                    // A non-empty reply body is the binder exception parcel
                    // (status word != 0): print its printable run so a guest
                    // exception is never mistaken for a silent delivery.
                    let text: String = frame
                        .data
                        .iter()
                        .filter(|b| (0x20..0x7f).contains(*b))
                        .map(|b| *b as char)
                        .collect();
                    eprintln!(
                        "acos-binder-peer: process {tag}: reverse delivery seq={} reply {} bytes: {}",
                        frame.seq,
                        frame.data.len(),
                        text
                    );
                }
            }
            other => {
                eprintln!("acos-binder-peer: ignoring frame type {other} from process {tag}");
            }
        }
        out.extend(self.take_delivery_frames());
        if let Some(frame) = self.take_launch_delivery(tag) {
            out.push((tag, frame));
        }
        out.extend(self.take_death_frames());
        out
    }

    // --- cross-process binder routing (MPROC3 step 3c) --------------------

    /// The synthetic node that stands for `(owner_tag, wire)` inside process
    /// `holder`, allocating it on first use.
    ///
    /// One node per `(owner, wire)` pair, whatever the number of holders: the
    /// pair *is* the object, and two holders must see two proxies to one object,
    /// not two objects. `None` means the bounded table is full, which the caller
    /// reports as `ART_RESOURCE_LIMIT` — never a fabricated handle.
    #[must_use]
    pub fn foreign_node(&mut self, owner_tag: u32, wire: u32, holder: u32) -> Option<u64> {
        let key = (owner_tag, wire);
        let node = match self.foreign_index.get(&key) {
            Some(node) => *node,
            None => {
                if self.foreign.len() >= MAX_FOREIGN_NODES {
                    self.resource_limits = self.resource_limits.saturating_add(1);
                    eprintln!(
                        "acos-binder-peer: ART_RESOURCE_LIMIT: foreign node table is full \u{2014} \
                         refusing to proxy (owner tag {owner_tag}, wire {wire})"
                    );
                    return None;
                }
                let node = FOREIGN_FLAG | self.foreign_next;
                self.foreign_next = self.foreign_next.wrapping_add(1).max(1);
                self.foreign_index.insert(key, node);
                self.foreign.insert(
                    node,
                    ForeignNode { owner_tag, wire, holders: Vec::new() },
                );
                node
            }
        };
        let owner_dead = !self.process(owner_tag).is_some_and(ProcessConn::connected);
        let entry = self.foreign.get_mut(&node).expect("just inserted");
        if !entry.holders.contains(&holder) {
            entry.holders.push(holder);
        }
        if owner_dead {
            // A proxy to an already-dead object: the death notification is due
            // as soon as the receiver holds it, exactly as a kernel driver
            // reports a dead binder on the first reference.
            self.queue_death(node, holder);
        }
        Some(node)
    }

    /// The `(owner tag, owner wire handle)` a synthetic node stands for.
    #[must_use]
    pub fn foreign_owner(&self, node: u64) -> Option<(u32, u32)> {
        self.foreign.get(&node).map(|n| (n.owner_tag, n.wire))
    }

    /// Every live foreign node, as `(node, owner tag, wire, holders)` — the
    /// evidence a reply/verdict carries.
    #[must_use]
    pub fn foreign_nodes(&self) -> Vec<(u64, u32, u32, Vec<u32>)> {
        let mut out: Vec<(u64, u32, u32, Vec<u32>)> = self
            .foreign
            .iter()
            .map(|(node, n)| (*node, n.owner_tag, n.wire, n.holders.clone()))
            .collect();
        out.sort_by_key(|x| x.0);
        out
    }

    /// How many synthetic nodes are live.
    #[must_use]
    pub fn foreign_node_count(&self) -> usize {
        self.foreign.len()
    }

    /// Rewrite a reply/delivery parcel so that process `dest` receives every
    /// binder as something that process can actually call.
    ///
    /// Public because it is the routing contract: every parcel the peer writes
    /// into a process's connection (host reply, reverse delivery, forwarded
    /// transaction, relayed reply) goes through here.
    ///
    /// A binder of `dest` itself stays a wire handle (its own handle space). A
    /// binder of another process becomes a synthetic node ([`FOREIGN_FLAG`]);
    /// the driver maps that node to a local handle, and a transaction on it comes
    /// back as an [`F_REQ`] with the node as `target`, which
    /// [`Self::forward_foreign`] forwards to the owner.
    pub fn export_for_process(&mut self, dest: u32, parcel: &Parcel) -> Parcel {
        let foreign: Vec<(BinderId, u32, u32)> = parcel
            .binder_ids()
            .into_iter()
            .filter_map(|id| {
                let owner = owner_process(id)?;
                if owner == dest {
                    return None;
                }
                let wire = java_wire(id)?;
                Some((id, owner, wire))
            })
            .collect();
        if foreign.is_empty() {
            return parcel.clone();
        }
        let mut nodes: HashMap<u64, u64> = HashMap::new();
        for (id, owner, wire) in &foreign {
            if let Some(node) = self.foreign_node(*owner, *wire, dest) {
                nodes.insert(id.0, node);
            }
        }
        if nodes.is_empty() {
            return parcel.clone();
        }
        // Rebuild the parcel with the node ids in place of the Java binder ids.
        // `frame_from_parcel` then writes `BINDER_TYPE_BINDER(node)` for a
        // `FOREIGN_FLAG` id (see its binder branch), which is what the driver
        // turns into a local handle for the receiver.
        let data = parcel.marshall();
        let mut out = data.clone();
        for &off in parcel.objects() {
            if off + 16 > out.len() {
                continue;
            }
            let id = u64::from_le_bytes(out[off + 8..off + 16].try_into().expect("id"));
            if let Some(node) = nodes.get(&id) {
                out[off..off + 4]
                    .copy_from_slice(&acos_parcel::binder::BINDER_TYPE_BINDER.to_le_bytes());
                out[off + 8..off + 16].copy_from_slice(&node.to_le_bytes());
            }
        }
        let offsets = parcel.objects().to_vec();
        let mut rewritten = Parcel::new();
        rewritten.unmarshall_with_objects(&out, &offsets);
        rewritten.set_data_position(0);
        rewritten
    }

    /// Forward a transaction addressed to a synthetic foreign node to the
    /// process that owns the binder.
    ///
    /// The caller waits for the reply: the peer remembers
    /// `(owner, forwarded seq) -> (caller, caller seq, node)` and relays the
    /// owner's [`F_REP`] back as the caller's [`F_RSP`]. A one-way transaction is
    /// fire-and-forget (the owner sends no reply for it). A node whose owner has
    /// no live connection is a structured refusal, never a hang and never a
    /// fabricated success.
    fn forward_foreign(&mut self, tag: u32, node: u64, frame: &Frame) -> (u32, Frame) {
        let Some((owner, wire)) = self.foreign_owner(node) else {
            let record = UnsupportedRecord {
                descriptor: "<foreign-node>".into(),
                code: frame.code,
                method: format!("code {}", frame.code),
                error: codes::SERVICE_UNKNOWN_BINDER.into(),
                message: format!("target {node:#x} is not a synthetic foreign node"),
            };
            self.log_unsupported(&record);
            return (
                tag,
                self.error_reply(frame, &ServiceError::UnknownBinder { handle: node }),
            );
        };
        if !self.process(owner).is_some_and(ProcessConn::connected) {
            self.foreign_dead = self.foreign_dead.saturating_add(1);
            let record = UnsupportedRecord {
                descriptor: "<foreign-node>".into(),
                code: frame.code,
                method: format!("code {}", frame.code),
                error: codes::SERVICE_UNKNOWN_BINDER.into(),
                message: format!(
                    "transaction on node {node:#x} (process {tag}) but its owner process \
                     {owner} has no connection"
                ),
            };
            self.log_unsupported(&record);
            let mut reply = self.error_reply(frame, &ServiceError::UnknownBinder { handle: node });
            reply.flags |= crate::protocol::RSP_DEAD;
            return (tag, reply);
        }
        let data = import_parcel_for(tag, frame);
        let exported = self.export_for_process(owner, &data);
        let one_way = frame.flags & TF_ONE_WAY != 0;
        let seq = self.next_txn_seq;
        self.next_txn_seq = self.next_txn_seq.wrapping_add(1).max(1);
        if !one_way {
            self.pending_foreign
                .insert((owner, seq), (tag, frame.seq, node));
        }
        self.foreign_forwards = self.foreign_forwards.saturating_add(1);
        let base = Frame {
            ty: F_TXN,
            seq,
            code: frame.code,
            flags: frame.flags,
            target: u64::from(wire),
            ..Default::default()
        };
        (owner, frame_from_parcel(F_TXN, &base, &exported))
    }

    /// Relay an owner's [`F_REP`] back to the process that called into it.
    ///
    /// Returns `None` when the reply belongs to a reverse delivery instead (the
    /// pre-MPROC3 case, which keeps its existing logging path).
    fn relay_foreign_reply(&mut self, owner: u32, frame: &Frame) -> Option<(u32, Frame)> {
        let (caller, caller_seq, node) = self.pending_foreign.remove(&(owner, frame.seq))?;
        if !self.process(caller).is_some_and(ProcessConn::connected) {
            return None;
        }
        if frame.flags & TF_STATUS_CODE != 0 {
            // libbinder never reached `onTransact` in the owner: the call failed.
            self.reverse_failed = self.reverse_failed.saturating_add(1);
            let base = Frame::plain(F_RSP, caller_seq, 0, 0, 0, Vec::new());
            let reply = self.error_reply(
                &base,
                &ServiceError::UnknownBinder { handle: node },
            );
            return Some((caller, reply));
        }
        let data = import_parcel_for(owner, frame);
        let exported = self.export_for_process(caller, &data);
        let base = Frame::plain(F_RSP, caller_seq, 0, 0, 0, Vec::new());
        Some((caller, frame_from_parcel(F_RSP, &base, &exported)))
    }

    /// Queue a [`crate::protocol::F_DEATH`] for one holder of a node.
    fn queue_death(&mut self, node: u64, holder: u32) {
        let frame = Frame::plain(crate::protocol::F_DEATH, 0, 0, 0, node, Vec::new());
        self.pending_deaths.push((holder, frame));
    }

    /// The death notifications the serve loop must write now.
    pub fn take_death_frames(&mut self) -> Vec<(u32, Frame)> {
        std::mem::take(&mut self.pending_deaths)
    }

    /// The owner of every foreign node this process holds died: notify the
    /// holders and refuse the transactions still waiting for an answer.
    ///
    /// The nodes stay in the table (a dead proxy is still a proxy: a late call
    /// on it must fail, not resolve to whatever reuses the number), but their
    /// `(owner, wire)` index entry goes, so a restarted owner's reused wire
    /// handle becomes a *new* object with a new node. Called from
    /// [`Self::disconnect_process`]; the notifications are queued for the serve
    /// loop, which writes them to the holders' connections.
    pub fn release_foreign_of(&mut self, tag: u32) -> usize {
        let owned: Vec<(u64, u32, u32)> = self
            .foreign
            .iter()
            .filter(|(_, n)| n.owner_tag == tag)
            .map(|(node, n)| (*node, n.owner_tag, n.wire))
            .collect();
        for (node, owner, wire) in &owned {
            self.foreign_index.remove(&(*owner, *wire));
            let holders = self
                .foreign
                .get(node)
                .map(|n| n.holders.clone())
                .unwrap_or_default();
            for holder in holders {
                self.queue_death(*node, holder);
            }
            // A node nobody holds any more cannot be reached again.
            if let Some(n) = self.foreign.get(node) {
                if n.holders.is_empty() {
                    self.foreign.remove(node);
                }
            }
        }
        // Requests still waiting for this owner can never be answered.
        let waiting: Vec<(u32, u32)> = self
            .pending_foreign
            .iter()
            .filter(|((owner, _), _)| *owner == tag)
            .map(|(key, _)| *key)
            .collect();
        for key in waiting {
            if let Some((caller, caller_seq, node)) = self.pending_foreign.remove(&key) {
                self.foreign_dead = self.foreign_dead.saturating_add(1);
                if self.process(caller).is_some_and(ProcessConn::connected) {
                    let base = Frame::plain(F_RSP, caller_seq, 0, 0, 0, Vec::new());
                    let mut reply =
                        self.error_reply(&base, &ServiceError::UnknownBinder { handle: node });
                    // The owner is dead: the caller must see BR_DEAD_REPLY.
                    reply.flags |= crate::protocol::RSP_DEAD;
                    self.pending_deaths.push((caller, reply));
                }
            }
        }
        owned.len()
    }

    /// Drop every reference process `tag` holds to other processes' binders
    /// (its connection is gone, so it can never release them itself).
    ///
    /// Returns the number of nodes freed.
    pub fn release_foreign_holders_of(&mut self, tag: u32) -> usize {
        let held: Vec<u64> = self
            .foreign
            .iter()
            .filter(|(_, n)| n.holders.contains(&tag))
            .map(|(node, _)| *node)
            .collect();
        let mut freed = 0;
        for node in held {
            if self.release_foreign_holder(node, tag) {
                freed += 1;
            }
        }
        freed
    }

    /// Release one holder's reference to a node (the driver's `BC_RELEASE`
    /// path). The node is freed once no process holds it; while any holder is
    /// left the same node id keeps naming the same object.
    ///
    /// Returns true when the node was freed.
    pub fn release_foreign_holder(&mut self, node: u64, holder: u32) -> bool {
        let Some(entry) = self.foreign.get_mut(&node) else {
            return false;
        };
        entry.holders.retain(|h| *h != holder);
        if !entry.holders.is_empty() {
            return false;
        }
        let (owner_tag, wire) = (entry.owner_tag, entry.wire);
        self.foreign.remove(&node);
        self.foreign_index.remove(&(owner_tag, wire));
        self.foreign_released = self.foreign_released.saturating_add(1);
        true
    }

    fn handle_request(&mut self, tag: u32, frame: &Frame) -> Option<Frame> {        if frame.code == PING_TRANSACTION {
            // `IBinder::PING_TRANSACTION` is answered with an empty parcel; a
            // reply header here would make libbinder read garbage (E3G §0).
            return Some(Frame::plain(
                F_RSP,
                frame.seq,
                0,
                frame.flags,
                frame.target,
                Vec::new(),
            ));
        }
        if frame.target == 0 {
            return Some(self.service_manager(tag, frame));
        }
        if frame.target & NODE_TAG != 0 {
            let host_id = frame.target & !NODE_TAG;
            if frame.code == INTERFACE_TRANSACTION {
                return Some(self.interface_transaction(frame, host_id));
            }
            if host_id == LAUNCH_NODE {
                return Some(self.launch_push(tag, frame));
            }
            return Some(self.service_transaction(tag, frame, host_id));
        }
        // A transaction for a handle the driver never mapped to a peer node.
        let (descriptor, _) = peek(frame);
        let record = UnsupportedRecord {
            descriptor: descriptor.unwrap_or_else(|| "<unknown>".into()),
            code: frame.code,
            method: format!("code {}", frame.code),
            error: codes::UNSUPPORTED_ANDROID_API.into(),
            message: format!("no service node for target {:#x}", frame.target),
        };
        self.log_unsupported(&record);
        Some(self.error_reply(frame, &ServiceError::UnknownBinder {
            handle: frame.target,
        }))
    }

    /// `IBinder::INTERFACE_TRANSACTION`: answer the node's AIDL descriptor.
    ///
    /// A real `BBinder` writes exactly one `String16` (no status word, no
    /// exception header) and libbinder's generated `asInterface` for the
    /// untabled native interfaces (`android.gui.*`) reads exactly that. An
    /// unknown node answers the empty string, which is what a `BBinder` with no
    /// descriptor set does — never a fabricated descriptor.
    fn interface_transaction(&mut self, frame: &Frame, host_id: u64) -> Frame {
        let descriptor = self
            .host
            .descriptor_of(BinderId(host_id))
            .unwrap_or_default();
        let mut reply = Parcel::new();
        if let Err(err) = reply.write_string16(descriptor) {
            return self.error_reply(
                frame,
                &ServiceError::Internal(format!("INTERFACE_TRANSACTION reply: {err}")),
            );
        }
        frame_from_parcel(F_RSP, frame, &reply)
    }

    fn service_manager(&mut self, tag: u32, frame: &Frame) -> Frame {
        // addService is the one IServiceManager call the service host cannot
        // answer: it registers a binder that lives in the *app* process.
        if frame.code == 3 {
            return self.add_service(tag, frame);
        }
        // `acos.launch` is a peer-owned node (the ART launch path needs one
        // binder the ServiceHost does not model); everything else is the host's.
        if frame.code == SM_GET_SERVICE || frame.code == SM_CHECK_SERVICE {
            let mut data = import_parcel_for(tag, frame);
            if let Ok(Some(name)) = data
                .enforce_interface("android.os.IServiceManager")
                .and_then(|()| data.read_string16())
            {
                if name == "acos.launch" {
                    let mut reply = Parcel::new();
                    exception::write_no_exception(&mut reply);
                    reply.write_strong_binder(BinderId(NODE_TAG | LAUNCH_NODE));
                    return frame_from_parcel(F_RSP, frame, &reply);
                }
            }
        }
        let mut data = import_parcel_for(tag, frame);
        let mut reply = Parcel::new();
        let Some(sm) = self.host.binder_for_descriptor("android.os.IServiceManager") else {
            return self.error_reply(
                frame,
                &ServiceError::Internal("IServiceManager binder is not registered".into()),
            );
        };
        match self.host.transact(sm, frame.code, &mut data, &mut reply, frame.flags) {
            Ok(()) => frame_from_parcel(F_RSP, frame, &reply),
            Err(err) => {
                self.record_error(frame, &mut data, &err);
                // `ServiceHost::transact` has already written the exception
                // reply for the failing call.
                frame_from_parcel(F_RSP, frame, &reply)
            }
        }
    }

    fn add_service(&mut self, tag: u32, frame: &Frame) -> Frame {
        let mut data = import_parcel_for(tag, frame);
        let mut reply = Parcel::new();
        let parsed = (|| -> Result<(String, BinderId), ServiceError> {
            data.enforce_interface("android.os.IServiceManager")?;
            let name = data.read_string16()?.unwrap_or_default();
            let binder = data.read_strong_binder()?;
            let _isolated = data.read_bool()?;
            let _priority = data.read_int32()?;
            Ok((name, binder.unwrap_or(BinderId::NULL)))
        })();
        match parsed {
            Ok((name, binder)) => {
                if binder.0 & JAVA_TAG != 0 {
                    let wire = (binder.0 & MASK32) as u32;
                    let count = self.java_binders_for(tag).len();
                    if !self.java_binders_for(tag).contains_key(&wire) && count >= MAX_JAVA_BINDERS {
                        let err = ServiceError::ResourceLimit {
                            what: "peer java binder table".into(),
                            limit: MAX_JAVA_BINDERS,
                        };
                        return self.limit_reply(frame, &err);
                    }
                    let id = java_binder_id(tag, wire);
                    self.host.set_binder_owner(id, tag);
                    if let Some(conn) = self
                        .conns
                        .get_mut(tag as usize)
                        .and_then(Option::as_mut)
                    {
                        conn.java_binders.insert(wire, name.clone());
                    }
                    eprintln!(
                        "acos-binder-peer: process {tag}: addService({name:?}) -> java binder wire={wire} 
                         ({} registered)",
                        self.java_binders_for(tag).len()
                    );
                } else {
                    eprintln!(
                        "acos-binder-peer: process {tag}: addService({name:?}) -> non-Java binder {:#x} recorded",
                        binder.0
                    );
                }
                exception::write_no_exception(&mut reply);
                frame_from_parcel(F_RSP, frame, &reply)
            }
            Err(err) => {
                self.record_error(frame, &mut data, &err);
                self.error_reply(frame, &err)
            }
        }
    }

    fn service_transaction(&mut self, tag: u32, frame: &Frame, host_id: u64) -> Frame {
        let binder = BinderId(host_id);
        let mut data = import_parcel_for(tag, frame);
        // Every Java binder this process hands the system in this transaction
        // is owned by this process: record that on the host, so a delivery to
        // it (or a later cleanup of the process) names the right connection.
        self.record_binder_owners(tag, &data);
        let mut reply = Parcel::new();
        let descriptor = data.peek_interface_token().ok().flatten();
        let method = descriptor
            .as_deref()
            .and_then(|d| acos_aidl::lookup(d, frame.code))
            .cloned();
        self.service_calls.push(ServiceCall {
            descriptor: descriptor.clone().unwrap_or_else(|| "<unknown>".into()),
            code: frame.code,
            method: method
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_else(|| format!("code {}", frame.code)),
        });
        self.bound_service_calls();
        // A guest-pushed, framework-marshalled reply (see `launch_push`). The
        // service call is still recorded above, so the transaction stays
        // visible in the minimal-surface list; only the parcel writer is the
        // framework's instead of `acos-services`'.
        if let (Some(d), Some(m)) = (descriptor.as_deref(), method.as_ref()) {
            if let Some(bytes) = self.launch_reply_for_tag(tag, d, frame.code) {
                eprintln!(
                    "acos-binder-peer: process {tag}: {d}.{} answered from a framework-marshalled \
                     reply ({} bytes)",
                    m.name,
                    bytes.len()
                );
                return Frame::plain(F_RSP, frame.seq, 0, 0, 0, bytes.to_vec());
            }
        }
        // The attach handshake is where the launch path learns the real
        // `IApplicationThread` binder it must answer with `bindApplication`.
        let attach_thread = if method.as_ref().map(|m| m.name.as_str()) == Some("attachApplication") {
            let mut args_data = import_parcel_for(tag, frame);
            descriptor
                .as_deref()
                .zip(method.as_ref())
                .and_then(|(d, m)| {
                    args_data.enforce_interface(d).ok()?;
                    acos_services::codec::read_args(&mut args_data, &m.args, d, &m.name).ok()
                })
                .and_then(|args| args.first().and_then(acos_services::value::Value::as_binder))
                .and_then(java_wire)
        } else {
            None
        };
        // `getDisplayInfo`: replay the reply the framework's own
        // `DisplayInfo.writeToParcel` produced in the guest. The service call is
        // still recorded above, so the transaction stays visible in the
        // minimal-surface list.
        if descriptor.as_deref() == Some("android.hardware.display.IDisplayManager")
            && method.as_ref().map(|m| m.name.as_str()) == Some("getDisplayInfo")
        {
            if let Some(bytes) = self
                .launch_state(tag)
                .and_then(|l| l.display_reply.clone())
            {
                eprintln!(
                    "acos-binder-peer: process {tag}: IDisplayManager.getDisplayInfo answered from \
                     the framework-marshalled reply ({} bytes)",
                    bytes.len()
                );
                return Frame::plain(F_RSP, frame.seq, 0, 0, 0, bytes);
            }
        }
        // `IWindowManager` is listed as `acos-wm`-owned in the registry, but the
        // host registers a real `WmsService` for the ServiceManager name
        // `window`; its scalar methods (`hasNavigationBar`, `useBLAST`, ...) are
        // answered by that object. Route those to the registered entry instead
        // of letting the ownership marker turn a real service into an error.
        let registered = descriptor.as_deref().and_then(|d| {
            if registry::owner_of(d).is_some() {
                self.host
                    .descriptors()
                    .into_iter()
                    .find(|x| *x == d)
                    .map(|d| (d.to_string(), true))
            } else {
                None
            }
        });
        let result = match registered {
            Some((descriptor, _)) => self.direct_dispatch(binder, &descriptor, frame.code, &mut data, &mut reply),
            None => self
                .host
                .transact(binder, frame.code, &mut data, &mut reply, frame.flags),
        };
        match result {
            Ok(()) => {
                if let Some(wire) = attach_thread {
                    // The process told the system which binder answers its
                    // deliveries: record it as owned by this process, so the
                    // launch delivery and every later callback are addressed to
                    // the connection that can actually receive them.
                    self.host.set_binder_owner(java_binder_id(tag, wire), tag);
                    if let Some(state) = self.launch_state_mut(tag) {
                        state.app_thread = Some(wire);
                    }
                }
                // Every binder this reply carries reaches the caller as
                // something it can call: its own wire handle for its own
                // binders, a synthetic node for another process's (MPROC3
                // step 3c). No fabricated handle can be produced here.
                let exported = self.export_for_process(tag, &reply);
                frame_from_parcel(F_RSP, frame, &exported)
            }
            Err(err) => {
                // This parcel has already been decoded (the descriptor and
                // method are known), so record *those* instead of re-peeking a
                // consumed parcel — that is what left `<unknown>` in the
                // evidence for every failing service call (P4a §5).
                self.record_error_at(descriptor.as_deref(), frame.code, &err);
                let reply = if reply.data_size() == 0 {
                    self.error_reply(frame, &err)
                } else {
                    // `ServiceHost::transact` wrote the exception header.
                    frame_from_parcel(F_RSP, frame, &reply)
                };
                reply
            }
        }
    }

    /// Dispatch to a service object the host has registered, bypassing the
    /// registry's "owned elsewhere" marker. Mirrors `ServiceHost::transact_inner`
    /// after the ownership check.
    fn direct_dispatch(
        &mut self,
        binder: BinderId,
        descriptor: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> acos_services::Result<()> {
        let method = acos_aidl::lookup(descriptor, code).ok_or_else(|| {
            ServiceError::UnsupportedAndroidApi {
                interface: descriptor.to_string(),
                code,
                method: format!("code {code}"),
            }
        })?;
        data.enforce_interface(descriptor)?;
        let args = acos_services::codec::read_args(data, &method.args, descriptor, &method.name)?;
        let object = self.host.ensure(
            self.host
                .descriptors()
                .into_iter()
                .find(|d| *d == descriptor)
                .ok_or_else(|| ServiceError::UnknownBinder { handle: binder.0 })?,
        )?;
        let call = object.call(descriptor, method, &args)?;
        // The host may have produced reverse deliveries; queueing them without a
        // ceiling is how a retry loop becomes a 36 GB host (R1).
        if self.local_deliveries.len() + call.deliveries.len() > MAX_PENDING_DELIVERIES {
            return Err(ServiceError::ResourceLimit {
                what: "peer delivery queue".into(),
                limit: MAX_PENDING_DELIVERIES,
            });
        }
        self.local_deliveries.extend(call.deliveries);
        reply.reset();
        exception::write_no_exception(reply);
        acos_services::codec::write_reply(reply, &method.reply, &call.reply, descriptor, &method.name)
    }

    fn error_reply(&self, frame: &Frame, err: &ServiceError) -> Frame {
        let mut reply = Parcel::new();
        acos_services::host::write_error_reply(&mut reply, err);
        frame_from_parcel(F_RSP, frame, &reply)
    }

    fn record_error(&mut self, frame: &Frame, data: &mut Parcel, err: &ServiceError) {
        let descriptor = data.peek_interface_token().ok().flatten();
        self.record_error_at(descriptor.as_deref(), frame.code, err);
    }

    /// Record a failing transaction whose descriptor is already known.
    fn record_error_at(&mut self, descriptor: Option<&str>, code: u32, err: &ServiceError) {
        let descriptor = descriptor.map_or_else(|| "<unknown>".to_string(), str::to_string);
        let method = acos_aidl::lookup(&descriptor, code)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| format!("code {code}"));
        let record = UnsupportedRecord {
            descriptor,
            code,
            method,
            error: err.code().to_string(),
            message: err.to_string(),
        };
        self.log_unsupported(&record);
    }

    fn log_unsupported(&mut self, record: &UnsupportedRecord) {
        // Keep the *first* records (they are the diagnosis of what the launch
        // needed), bound the in-memory copy and the JSONL file, and count what
        // was refused. A guest cannot turn this log into unbounded memory/disk.
        if self.unsupported.len() >= MAX_UNSUPPORTED_RECORDS {
            self.unsupported_dropped = self.unsupported_dropped.saturating_add(1);
            self.resource_limits = self.resource_limits.saturating_add(1);
            if self.unsupported_dropped == 1 {
                eprintln!(
                    "acos-binder-peer: ART_RESOURCE_LIMIT: unsupported-record log is full \
                     ({MAX_UNSUPPORTED_RECORDS} records); further records are counted only"
                );
            }
            return;
        }
        let line = record.to_json();
        eprintln!("acos-binder-peer: UNSUPPORTED_ANDROID_API {line}");
        if let Some(file) = self.log.as_mut() {
            if self.log_lines < MAX_UNSUPPORTED_RECORDS {
                self.log_lines += 1;
                let _ = writeln!(file, "{line}");
                if self.log_lines == MAX_UNSUPPORTED_RECORDS {
                    let _ = writeln!(
                        file,
                        "{{\"kind\":\"ART_RESOURCE_LIMIT\",\"limit\":{MAX_UNSUPPORTED_RECORDS}}}"
                    );
                }
                let _ = file.flush();
            }
        }
        self.unsupported.push(record.clone());
    }

    /// Keep the forwarded-transaction evidence bounded (drop-oldest, counted).
    fn bound_service_calls(&mut self) {
        if self.service_calls.len() > MAX_SERVICE_CALLS {
            let drop_n = self.service_calls.len() - MAX_SERVICE_CALLS;
            self.service_calls.drain(..drop_n);
            self.resource_limits = self.resource_limits.saturating_add(drop_n as u64);
        }
    }

    /// A reply that refuses the request with `ART_RESOURCE_LIMIT`, recording it so
    /// the overflow is visible in the run's evidence (never a silent drop).
    fn limit_reply(&mut self, frame: &Frame, err: &ServiceError) -> Frame {
        self.resource_limits = self.resource_limits.saturating_add(1);
        eprintln!("acos-binder-peer: {}: {err}", err.code());
        let record = UnsupportedRecord {
            descriptor: "<peer>".into(),
            code: frame.code,
            method: format!("code {}", frame.code),
            error: err.code().to_string(),
            message: err.to_string(),
        };
        self.log_unsupported(&record);
        self.error_reply(frame, err)
    }

    /// `acos.launch` node: the guest pushes the marshalled `bindApplication`
    /// request (code 1) and framework-marshalled reply parcels (code 2). Both
    /// are replayed verbatim later; nothing is invented in Rust.
    fn launch_push(&mut self, tag: u32, frame: &Frame) -> Frame {
        let mut data = import_parcel_for(tag, frame);
        let mut reply = Parcel::new();
        let parsed = (|| -> Result<(Option<(String, u32)>, Vec<u8>), ServiceError> {
            let mut key = None;
            if frame.code == LAUNCH_PUSH_REPLY {
                // The request's interface token *is* the target descriptor:
                // strict-mode policy, work-source uid, token header, string16.
                let _policy = data.read_int32()?;
                let _work_source = data.read_int32()?;
                let _header = data.read_int32()?;
                let descriptor = data.read_string16()?.ok_or_else(|| ServiceError::BadRequest {
                    interface: "dev.acos.art.ILaunch".into(),
                    method: "pushReply".into(),
                    detail: "missing descriptor".into(),
                })?;
                let code = data.read_int32()? as u32;
                key = Some((descriptor, code));
            } else {
                data.enforce_interface("dev.acos.art.ILaunch")?;
            }
            let bytes = data.read_byte_array()?.ok_or_else(|| ServiceError::BadRequest {
                interface: "dev.acos.art.ILaunch".into(),
                method: "pushBindApplication".into(),
                detail: "missing parcel bytes".into(),
            })?;
            Ok((key, bytes))
        })();
        match parsed {
            Ok((Some((descriptor, code)), bytes)) => {
                let used = self.launch_state(tag).map_or(0, |l| l.replies.len());
                let known = self
                    .launch_state(tag)
                    .is_some_and(|l| l.replies.contains_key(&(descriptor.clone(), code)));
                if used >= MAX_LAUNCH_REPLIES && !known {
                    let err = ServiceError::ResourceLimit {
                        what: "peer launch reply table".into(),
                        limit: MAX_LAUNCH_REPLIES,
                    };
                    return self.limit_reply(frame, &err);
                }
                eprintln!(
                    "acos-binder-peer: process {tag}: launch reply template stashed for 
                     {descriptor} code {code} ({} bytes, framework-marshalled)",
                    bytes.len()
                );
                let is_display = descriptor == "android.hardware.display.IDisplayManager" && code == 1;
                if let Some(state) = self.launch_state_mut(tag) {
                    if is_display {
                        state.display_reply = Some(bytes.clone());
                    }
                    state.replies.insert((descriptor, code), bytes);
                }
                exception::write_no_exception(&mut reply);
                frame_from_parcel(F_RSP, frame, &reply)
            }
            Ok((None, bytes)) => {
                let is_txn = frame.code == LAUNCH_PUSH_TRANSACTION;
                if is_txn {
                    eprintln!(
                        "acos-binder-peer: process {tag}: launch ClientTransaction request stashed 
                         ({} bytes, framework-marshalled)",
                        bytes.len()
                    );
                } else {
                    eprintln!(
                        "acos-binder-peer: process {tag}: launch bindApplication request stashed 
                         ({} bytes)",
                        bytes.len()
                    );
                }
                if let Some(state) = self.launch_state_mut(tag) {
                    if is_txn {
                        state.txn = Some(bytes);
                    } else {
                        state.bind = Some(bytes);
                    }
                }
                exception::write_no_exception(&mut reply);
                frame_from_parcel(F_RSP, frame, &reply)
            }
            Err(err) => {
                self.record_error(frame, &mut data, &err);
                self.error_reply(frame, &err)
            }
        }
    }

    /// Queue the one `bindApplication` delivery for process `tag`, after its
    /// `attachApplication`, then the one `scheduleTransaction` delivery once the
    /// guest pushed it (P4b).
    fn take_launch_delivery(&mut self, tag: u32) -> Option<Frame> {
        // Take the `seq` before borrowing the state: both are fields of
        // `self`.
        let seq = self.next_launch_seq();
        let state = self.launch_state_mut(tag)?;
        let wire = state.app_thread?;
        if !state.delivered {
            let bytes = state.bind.clone()?;
            state.delivered = true;
            eprintln!(
                "acos-binder-peer: process {tag}: delivering bindApplication ({} bytes, code {}) to 
                 java binder wire={wire}",
                bytes.len(),
                BIND_APPLICATION_CODE
            );
            return Some(Frame::plain(
                F_TXN,
                seq,
                BIND_APPLICATION_CODE,
                0,
                u64::from(wire),
                bytes,
            ));
        }
        if state.txn_delivered {
            return None;
        }
        let bytes = state.txn.clone()?;
        state.txn_delivered = true;
        eprintln!(
            "acos-binder-peer: process {tag}: delivering ClientTransaction ({} bytes, \
             IApplicationThread.scheduleTransaction code {}) to java binder wire={wire}",
            bytes.len(),
            SCHEDULE_TRANSACTION_CODE
        );
        Some(Frame::plain(
            F_TXN,
            seq,
            SCHEDULE_TRANSACTION_CODE,
            0,
            u64::from(wire),
            bytes,
        ))
    }

    fn next_launch_seq(&mut self) -> u32 {
        let seq = self.next_txn_seq;
        self.next_txn_seq = self.next_txn_seq.wrapping_add(1).max(1);
        seq
    }

    /// Reverse deliveries with the tag of the connection each must be written
    /// to.
    ///
    /// The tag comes from the target binder id ([`owner_process`]), which is why
    /// a `JAVA_TAG` id carries its process: two processes can both have driver
    /// wire handle 7, and a delivery must reach the one the system meant.
    fn take_delivery_frames(&mut self) -> Vec<(u32, Frame)> {
        let mut pending = std::mem::take(&mut self.local_deliveries);
        pending.extend(self.host.take_deliveries());
        // Defence in depth: even the frames produced in one turn are bounded, so a
        // single transaction that fans out into thousands of reverse deliveries
        // cannot grow this turn's `out` without limit.
        if pending.len() > MAX_PENDING_DELIVERIES {
            let dropped = pending.len() - MAX_PENDING_DELIVERIES;
            self.resource_limits = self.resource_limits.saturating_add(dropped as u64);
            self.unsupported_dropped = self.unsupported_dropped.saturating_add(dropped as u64);
            eprintln!(
                "acos-binder-peer: ART_RESOURCE_LIMIT: {dropped} reverse deliveries dropped \
                 (cap {MAX_PENDING_DELIVERIES} per turn)"
            );
            pending.truncate(MAX_PENDING_DELIVERIES);
        }
        let mut out = Vec::new();
        for d in pending {
            let Some(wire) = java_wire(d.target) else {
                let record = UnsupportedRecord {
                    descriptor: d.interface.clone(),
                    code: d.code,
                    method: d.method.clone(),
                    error: codes::UNSUPPORTED_ANDROID_API.into(),
                    message: format!(
                        "delivery target {:#x} is not a registered Java binder",
                        d.target.0
                    ),
                };
                self.log_unsupported(&record);
                continue;
            };
            let tag = owner_process(d.target).unwrap_or(crate::protocol::TAG_PRIMARY);
            if !self.process(tag).is_some_and(ProcessConn::connected) {
                let record = UnsupportedRecord {
                    descriptor: d.interface.clone(),
                    code: d.code,
                    method: d.method.clone(),
                    error: codes::UNSUPPORTED_ANDROID_API.into(),
                    message: format!(
                        "delivery target {:#x} belongs to process {tag}, which has no connection",
                        d.target.0
                    ),
                };
                self.log_unsupported(&record);
                continue;
            }
            let seq = self.next_txn_seq;
            self.next_txn_seq = self.next_txn_seq.wrapping_add(1).max(1);
            let exported = self.export_for_process(tag, &d.data);
            let mut frame = frame_from_parcel(F_TXN, &Frame { seq, ..Default::default() }, &exported);
            frame.target = u64::from(wire);
            frame.code = d.code;
            frame.flags = 0;
            out.push((tag, frame));
        }
        out
    }

    /// Record the host-side owner of every Java binder a process just handed to
    /// the system in this request parcel.
    fn record_binder_owners(&mut self, tag: u32, data: &Parcel) {
        for id in data.binder_ids() {
            if id.0 & JAVA_TAG != 0 {
                self.host.set_binder_owner(id, tag);
            }
        }
    }
}

impl Default for Peer {
    fn default() -> Self {
        Self::new()
    }
}

/// `BinderId` of a Java local binder that belongs to app process `tag`.
#[must_use]
pub fn java_binder_id(tag: u32, wire: u32) -> BinderId {
    BinderId(JAVA_TAG | ((u64::from(tag) & PROC_MASK) << PROC_SHIFT) | u64::from(wire))
}

/// The app process that owns a Java local binder id (`None` for a host node).
#[must_use]
pub fn owner_process(id: BinderId) -> Option<u32> {
    if id.0 & JAVA_TAG == 0 {
        return None;
    }
    Some(((id.0 >> PROC_SHIFT) & PROC_MASK) as u32)
}

/// `BinderId` -> driver wire handle when it is a Java local binder.
#[must_use]
pub fn java_wire(id: BinderId) -> Option<u32> {
    if id.0 & JAVA_TAG != 0 {
        Some((id.0 & MASK32) as u32)
    } else {
        None
    }
}

/// `BinderId` -> peer node id. Host ids are tagged; already-tagged ids pass.
#[must_use]
pub fn peer_node(id: BinderId) -> u64 {
    if id.0 & NODE_TAG != 0 {
        id.0
    } else {
        NODE_TAG | id.0
    }
}

fn peek(frame: &Frame) -> (Option<String>, usize) {
    let mut p = import_parcel_raw(crate::protocol::TAG_PRIMARY, frame);
    let d = p.peek_interface_token().ok().flatten();
    (d, p.data_size())
}

/// Import a frame's parcel into an `acos-parcel` `Parcel` (primary process).
#[must_use]
pub fn import_parcel(frame: &Frame) -> Parcel {
    import_parcel_for(crate::protocol::TAG_PRIMARY, frame)
}

/// Import a frame's parcel for the connection of process `tag`.
///
/// Binder objects are rewritten from the driver's wire form to the peer's
/// `BinderId` form (`JAVA_TAG`/`NODE_TAG`), and `BINDER_TYPE_FD` objects get the
/// descriptor that arrived in `SCM_RIGHTS` (one per object, in order).
///
/// A `BINDER_TYPE_HANDLE` is a driver wire handle, which is only meaningful
/// inside the process that wrote it: the id carries `tag` so two processes'
/// handles never collide (MPROC2 step 3a).
#[must_use]
pub fn import_parcel_for(tag: u32, frame: &Frame) -> Parcel {
    import_parcel_raw(tag, frame)
}

fn import_parcel_raw(tag: u32, frame: &Frame) -> Parcel {
    // libbinder writes a 4-byte stability record right after every
    // `flat_binder_object` (`Parcel::unflattenBinder` reads it back). The
    // in-process `acos-parcel` format does not carry it, so strip it here and
    // rebase the object offsets. Without this every binder reads as null and
    // the following argument is misaligned.
    let mut out = Vec::with_capacity(frame.data.len());
    // Object offsets in the *rebased* stream, so the imported parcel keeps a
    // real object table (a caller can then ask which binders it carries).
    let mut offsets = Vec::with_capacity(frame.object_offsets().len());
    let mut cursor = 0usize;
    let mut fds = frame.fds.iter().copied();
    for off in frame.object_offsets() {
        if off < cursor || off + 24 > frame.data.len() {
            continue;
        }
        out.extend_from_slice(&frame.data[cursor..off]);
        let ty = u32::from_le_bytes(frame.data[off..off + 4].try_into().expect("type"));
        let id = u64::from_le_bytes(frame.data[off + 8..off + 16].try_into().expect("id"));
        let new_id = match ty {
            acos_parcel::binder::BINDER_TYPE_BINDER
            | acos_parcel::binder::BINDER_TYPE_WEAK_BINDER => {
                if id == 0 {
                    0
                } else if id & NODE_TAG != 0 {
                    id & !NODE_TAG
                } else {
                    // A bare node id (older driver): a host id, never Java.
                    id
                }
            }
            acos_parcel::binder::BINDER_TYPE_HANDLE
            | acos_parcel::binder::BINDER_TYPE_WEAK_HANDLE => {
                if id == 0 {
                    0
                } else {
                    java_binder_id(tag, (id & MASK32) as u32).0
                }
            }
            BINDER_TYPE_FD => fds.next().map_or(id, |fd| fd as u64),
            _ => id,
        };
        let mut obj = [0u8; 24];
        obj.copy_from_slice(&frame.data[off..off + 24]);
        obj[8..16].copy_from_slice(&new_id.to_le_bytes());
        // A null binder is listed in the wire offsets here (the peer wrote it),
        // and `binder_ids` skips it; keeping the entry preserves the table.
        offsets.push(out.len());
        out.extend_from_slice(&obj);
        let mut next = off + 24;
        let binder = matches!(
            ty,
            acos_parcel::binder::BINDER_TYPE_BINDER
                | acos_parcel::binder::BINDER_TYPE_WEAK_BINDER
                | acos_parcel::binder::BINDER_TYPE_HANDLE
                | acos_parcel::binder::BINDER_TYPE_WEAK_HANDLE
        );
        if binder && next + 4 <= frame.data.len() {
            next += 4;
        }
        cursor = next;
    }
    out.extend_from_slice(&frame.data[cursor..]);
    let mut parcel = Parcel::new();
    parcel.unmarshall_with_objects(&out, &offsets);
    parcel.set_data_position(0);
    // The guest's libbinder wrote this parcel: a null binder carries the 4-byte
    // stability record but no object-table entry (L1 §3.6).
    parcel.set_wire_null_stability(true);
    parcel
}

/// Serialise a host reply/delivery parcel into a frame with the driver's wire
/// encoding.
#[must_use]
pub fn frame_from_parcel(ty: u32, base: &Frame, parcel: &Parcel) -> Frame {
    let data = parcel.marshall();
    // Rebuild in wire form: every binder object is followed by its 4-byte
    // stability record, and later object offsets shift accordingly.
    let mut out = Vec::with_capacity(data.len() + parcel.objects().len() * 4);
    let mut offs = Vec::with_capacity(parcel.objects().len() * 8);
    let mut fds = Vec::new();
    let mut cursor = 0usize;
    for &o in parcel.objects() {
        if o < cursor || o + 24 > data.len() {
            continue;
        }
        out.extend_from_slice(&data[cursor..o]);
        let mut obj = [0u8; 24];
        obj.copy_from_slice(&data[o..o + 24]);
        let parsed = FlatObject::from_bytes(&obj);
        let mut add_stability = false;
        if parsed.is_file_descriptor() {
            if parsed.id != 0 && fds.len() < crate::protocol::MAX_FDS {
                // The descriptor the object records is owned by `parcel`, which
                // the caller drops as soon as this frame exists (the reply
                // `Parcel` lives for one transaction). Handing that number to
                // `write_frame` closes nothing but the fd is **already gone** by
                // then: `sendmsg` fails with `EBADF` and the guest's
                // `transact` waits for a reply that can never arrive. Duplicate
                // it so the frame owns a live descriptor until it is written
                // (see `close_frame_fds`).
                let dup = unsafe { libc::fcntl(parsed.id as i32, libc::F_DUPFD_CLOEXEC, 0) };
                if dup >= 0 {
                    fds.push(dup);
                } else {
                    eprintln!(
                        "acos-binder-peer: ART_FD_UNAVAILABLE: cannot duplicate fd {} for the 
                         reply frame: {}",
                        parsed.id,
                        std::io::Error::last_os_error()
                    );
                }
            }
        } else if parsed.is_binder() {
            add_stability = true;
            let (tag, id) = if parsed.id == 0 {
                (acos_parcel::binder::BINDER_TYPE_BINDER, 0u64)
            } else if parsed.id & NODE_TAG != 0 {
                (acos_parcel::binder::BINDER_TYPE_BINDER, parsed.id)
            } else if parsed.id & FOREIGN_FLAG != 0 {
                // A synthetic node for a binder that lives in another app
                // process (MPROC3 step 3c). It must reach the receiver as a
                // *node* (`BINDER_TYPE_BINDER`), never as a handle: its low
                // bits are the peer's node number, not a driver handle.
                (acos_parcel::binder::BINDER_TYPE_BINDER, parsed.id)
            } else if let Some(wire) = java_wire(BinderId(parsed.id)) {
                (acos_parcel::binder::BINDER_TYPE_HANDLE, u64::from(wire))
            } else {
                (
                    acos_parcel::binder::BINDER_TYPE_BINDER,
                    peer_node(BinderId(parsed.id)),
                )
            };
            obj[0..4].copy_from_slice(&tag.to_le_bytes());
            obj[4..8]
                .copy_from_slice(&acos_parcel::binder::STRONG_BINDER_FLAGS.to_le_bytes());
            obj[8..16].copy_from_slice(&id.to_le_bytes());
            obj[16..24].copy_from_slice(&0u64.to_le_bytes());
        }
        // libbinder does not list a **null** binder in the offset table
        // (`Parcel::writeObject(obj, /*nullMetaData=*/false)` skips the entry when
        // `binder == 0`), although it still writes the 24-byte object and the
        // 4-byte stability record. Match that exactly: the guest reads a null
        // binder by position, not from the object table, and an extra entry
        // would make the table disagree with what the receiver walks.
        let null_binder = parsed.is_binder() && parsed.id == 0;
        if !null_binder {
            offs.extend_from_slice(&(out.len() as u64).to_le_bytes());
        }
        out.extend_from_slice(&obj);
        if add_stability {
            // Stability::Level::SYSTEM == 0b1100 (E3G §3.3) for a real binder,
            // but a **null** binder is always written with `UNDECLARED` (0):
            // libbinder's `finishUnflattenBinder` feeds this int to
            // `Stability::setRepr(nullptr, level, log=true)`, and any level
            // other than UNDECLARED makes it return `BAD_TYPE`. The whole
            // reply then fails to parse, which is why a `getService` miss
            // (a null binder) surfaced as
            // `Status(-129, EX_TRANSACTION_FAILED): 'BAD_TYPE: '` instead of
            // `null` (L1 §2).
            let stability: i32 = if parsed.id == 0 { 0 } else { 12 };
            out.extend_from_slice(&stability.to_le_bytes());
        }
        cursor = o + 24;
    }
    out.extend_from_slice(&data[cursor..]);

    // F_RSP's `code` is the driver's status word (0 = ok); it is *not* the
    // transaction code.  E3G's peer sent 0 here and a non-zero value makes
    // libbinder report BR_FAILED_REPLY (`!!! FAILED BINDER TRANSACTION !!!`).
    let (code, flags, target) = if ty == F_RSP {
        (0, 0, 0)
    } else {
        (base.code, base.flags, base.target)
    };
    Frame {
        ty,
        seq: base.seq,
        code,
        flags,
        target,
        data: out,
        offs,
        fds,
    }
}

/// The kind the driver should use for a host service binder.
#[must_use]
pub fn host_binder_kind(id: BinderId) -> BinderKind {
    if java_wire(id).is_some() {
        BinderKind::Proxy
    } else {
        BinderKind::Local
    }
}
