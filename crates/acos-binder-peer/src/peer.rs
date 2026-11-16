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
    Frame, F_REP, F_REQ, F_RSP, F_TXN, INTERFACE_TRANSACTION, PING_TRANSACTION, TF_ONE_WAY,
    TF_STATUS_CODE,
};

/// Bit 32 marks a driver wire handle (a Java local binder).
pub const JAVA_TAG: u64 = 1 << 32;
/// Bit 33 marks one of this peer's service nodes.
pub const NODE_TAG: u64 = 1 << 33;
/// The `acos.launch` peer node (P4a): the ART guest pushes the marshalled
/// `IApplicationThread.bindApplication` request here, and the peer delivers it
/// to the real `ApplicationThread` binder after `attachApplication`.
pub const LAUNCH_NODE: u64 = 1 << 40;

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

/// The peer. One instance serves one driver connection.
pub struct Peer {
    host: ServiceHost,
    log_path: Option<PathBuf>,
    log: Option<File>,
    /// Driver wire handle -> service name registered with `addService`.
    java_binders: HashMap<u32, String>,
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
    /// `IApplicationThread.bindApplication` bytes stashed by the guest.
    launch_bind: Option<Vec<u8>>,
    /// The `IDisplayManager.getDisplayInfo` reply, marshalled by the framework's
    /// own `DisplayInfo.writeToParcel` in the guest.
    launch_display_reply: Option<Vec<u8>>,
    /// Framework-marshalled reply parcels keyed by `(descriptor, code)`.
    launch_replies: HashMap<(String, u32), Vec<u8>>,
    /// The `IApplicationThread` wire handle named by `attachApplication`.
    launch_app_thread: Option<u32>,
    /// The launch delivery was queued (once).
    launch_delivered: bool,
    /// The `IApplicationThread.scheduleTransaction` request bytes stashed by the
    /// guest (one `ClientTransaction`, framework-marshalled).
    launch_txn: Option<Vec<u8>>,
    /// The transaction delivery was queued (once).
    launch_txn_delivered: bool,
    /// Reverse replies received from Java (peer-initiated transactions are
    /// two-way here, matching the runtime's `BinderDelivery`).
    pub reverse_replies: u64,
    /// How many of those replies were `TF_STATUS_CODE` failures, i.e. libbinder
    /// never dispatched the transaction to `onTransact` (typically
    /// `UNKNOWN_TRANSACTION` from a node that was not rooted). Counting these
    /// separately is what stops "20 replies" from being read as "20 deliveries".
    pub reverse_failed: u64,
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer")
            .field("host", &self.host)
            .field("java_binders", &self.java_binders)
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
        Self {
            host,
            log_path,
            log,
            java_binders: HashMap::new(),
            local_deliveries: Vec::new(),
            next_txn_seq: 1,
            unsupported: Vec::new(),
            service_calls: Vec::new(),
            resource_limits: 0,
            unsupported_dropped: 0,
            log_lines: 0,
            launch_bind: None,
            launch_display_reply: None,
            launch_replies: HashMap::new(),
            launch_app_thread: None,
            launch_delivered: false,
            launch_txn: None,
            launch_txn_delivered: false,
            reverse_replies: 0,
            reverse_failed: 0,
        }
    }

    /// The host (tests/inspection).
    #[must_use]
    pub fn host(&self) -> &ServiceHost {
        &self.host
    }

    /// The host, mutably: a live session's module attach/detach is a service
    /// table mutation and the peer thread is the table's owner.
    #[must_use]
    pub fn host_mut(&mut self) -> &mut ServiceHost {
        &mut self.host
    }

    /// Path of the JSON-lines unsupported log, when enabled.
    #[must_use]
    pub fn log_path(&self) -> Option<&Path> {
        self.log_path.as_deref()
    }

    /// Java binders registered through `addService` (`wire handle -> name`).
    #[must_use]
    pub fn java_binders(&self) -> &HashMap<u32, String> {
        &self.java_binders
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
        self.launch_bind.as_ref().map(Vec::len)
    }

    /// The `attachApplication` binder wire handle.
    #[must_use]
    pub fn launch_app_thread(&self) -> Option<u32> {
        self.launch_app_thread
    }

    /// True when `bindApplication` was delivered to the app thread.
    #[must_use]
    pub fn launch_delivered(&self) -> bool {
        self.launch_delivered
    }

    /// The guest pushed a marshalled `ClientTransaction`.
    #[must_use]
    pub fn launch_txn_len(&self) -> Option<usize> {
        self.launch_txn.as_ref().map(Vec::len)
    }

    /// True when the `scheduleTransaction` delivery was queued.
    #[must_use]
    pub fn launch_txn_delivered(&self) -> bool {
        self.launch_txn_delivered
    }

    /// The framework-marshalled `getDisplayInfo` reply, if the guest pushed one.
    #[must_use]
    pub fn launch_display_reply(&self) -> Option<&[u8]> {
        self.launch_display_reply.as_deref()
    }

    /// A framework-marshalled reply pushed by the guest for `(descriptor, code)`.
    #[must_use]
    pub fn launch_reply_for(&self, descriptor: &str, code: u32) -> Option<&[u8]> {
        self.launch_replies
            .get(&(descriptor.to_string(), code))
            .map(Vec::as_slice)
    }

    /// Handle one incoming frame and return the frames to send back.
    pub fn handle(&mut self, frame: Frame) -> Vec<Frame> {
        let mut out = Vec::new();
        match frame.ty {
            F_REQ => {
                if let Some(reply) = self.handle_request(&frame) {
                    if frame.flags & TF_ONE_WAY == 0 {
                        out.push(reply);
                    }
                }
            }
            F_REP => {
                self.reverse_replies += 1;
                if frame.flags & TF_STATUS_CODE != 0 {
                    self.reverse_failed += 1;
                    eprintln!(
                        "acos-binder-peer: reverse delivery seq={} was NOT dispatched \
                         (TF_STATUS_CODE reply: libbinder never reached onTransact)",
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
                        "acos-binder-peer: reverse delivery seq={} reply {} bytes: {}",
                        frame.seq,
                        frame.data.len(),
                        text
                    );
                    // A `system -> app` delivery the app's `onTransact` rejected
                    // (a non-empty reply body is a binder exception parcel) is
                    // evidence, not a log line: record it next to the
                    // unsupported-transaction records so a verdict can see that
                    // the callback never ran.
                    let record = serde_json::json!({
                        "kind": "REVERSE_DELIVERY_EXCEPTION",
                        "seq": frame.seq,
                        "bytes": frame.data.len(),
                        "text": text,
                    })
                    .to_string();
                    self.log_line(&record);
                }
            }
            other => {
                eprintln!("acos-binder-peer: ignoring frame type {other}");
            }
        }
        out.extend(self.take_delivery_frames());
        if let Some(frame) = self.take_launch_delivery() {
            out.push(frame);
        }
        out
    }

    fn handle_request(&mut self, frame: &Frame) -> Option<Frame> {
        if frame.code == PING_TRANSACTION {
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
            return Some(self.service_manager(frame));
        }
        if frame.target & NODE_TAG != 0 {
            let host_id = frame.target & !NODE_TAG;
            if frame.code == INTERFACE_TRANSACTION {
                return Some(self.interface_transaction(frame, host_id));
            }
            if host_id == LAUNCH_NODE {
                return Some(self.launch_push(frame));
            }
            return Some(self.service_transaction(frame, host_id));
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

    fn service_manager(&mut self, frame: &Frame) -> Frame {
        // addService is the one IServiceManager call the service host cannot
        // answer: it registers a binder that lives in the *app* process.
        if frame.code == 3 {
            return self.add_service(frame);
        }
        // `acos.launch` is a peer-owned node (the ART launch path needs one
        // binder the ServiceHost does not model); everything else is the host's.
        if frame.code == SM_GET_SERVICE || frame.code == SM_CHECK_SERVICE {
            let mut data = import_parcel(frame);
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
        let mut data = import_parcel(frame);
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

    fn add_service(&mut self, frame: &Frame) -> Frame {
        let mut data = import_parcel(frame);
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
                    if !self.java_binders.contains_key(&wire)
                        && self.java_binders.len() >= MAX_JAVA_BINDERS
                    {
                        let err = ServiceError::ResourceLimit {
                            what: "peer java binder table".into(),
                            limit: MAX_JAVA_BINDERS,
                        };
                        return self.limit_reply(frame, &err);
                    }
                    self.java_binders.insert(wire, name.clone());
                    eprintln!(
                        "acos-binder-peer: addService({name:?}) -> java binder wire={wire} ({} registered)",
                        self.java_binders.len()
                    );
                } else {
                    eprintln!(
                        "acos-binder-peer: addService({name:?}) -> non-Java binder {:#x} recorded",
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

    fn service_transaction(&mut self, frame: &Frame, host_id: u64) -> Frame {
        let binder = BinderId(host_id);
        let mut data = import_parcel(frame);
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
            if let Some(bytes) = self.launch_reply_for(d, frame.code) {
                eprintln!(
                    "acos-binder-peer: {d}.{} answered from a framework-marshalled reply ({} bytes)",
                    m.name,
                    bytes.len()
                );
                return Frame::plain(F_RSP, frame.seq, 0, 0, 0, bytes.to_vec());
            }
        }
        // The attach handshake is where the launch path learns the real
        // `IApplicationThread` binder it must answer with `bindApplication`.
        let attach_thread = if method.as_ref().map(|m| m.name.as_str()) == Some("attachApplication") {
            let mut args_data = import_parcel(frame);
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
            if let Some(bytes) = self.launch_display_reply.clone() {
                eprintln!(
                    "acos-binder-peer: IDisplayManager.getDisplayInfo answered from the framework-marshalled reply ({} bytes)",
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
                    self.launch_app_thread = Some(wire);
                }
                frame_from_parcel(F_RSP, frame, &reply)
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

    fn log_line(&mut self, line: &str) {
        if let Some(log) = self.log.as_mut() {
            let _ = writeln!(log, "{line}");
        }
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
    fn launch_push(&mut self, frame: &Frame) -> Frame {
        let mut data = import_parcel(frame);
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
                if self.launch_replies.len() >= MAX_LAUNCH_REPLIES
                    && !self.launch_replies.contains_key(&(descriptor.clone(), code))
                {
                    let err = ServiceError::ResourceLimit {
                        what: "peer launch reply table".into(),
                        limit: MAX_LAUNCH_REPLIES,
                    };
                    return self.limit_reply(frame, &err);
                }
                eprintln!(
                    "acos-binder-peer: launch reply template stashed for {descriptor} code {code} ({} bytes, framework-marshalled)",
                    bytes.len()
                );
                if descriptor == "android.hardware.display.IDisplayManager" && code == 1 {
                    self.launch_display_reply = Some(bytes.clone());
                }
                self.launch_replies.insert((descriptor, code), bytes);
                exception::write_no_exception(&mut reply);
                frame_from_parcel(F_RSP, frame, &reply)
            }
            Ok((None, bytes)) => {
                if frame.code == LAUNCH_PUSH_TRANSACTION {
                    eprintln!(
                        "acos-binder-peer: launch ClientTransaction request stashed ({} bytes, framework-marshalled)",
                        bytes.len()
                    );
                    self.launch_txn = Some(bytes);
                    exception::write_no_exception(&mut reply);
                    return frame_from_parcel(F_RSP, frame, &reply);
                }
                eprintln!(
                    "acos-binder-peer: launch bindApplication request stashed ({} bytes)",
                    bytes.len()
                );
                self.launch_bind = Some(bytes);
                exception::write_no_exception(&mut reply);
                frame_from_parcel(F_RSP, frame, &reply)
            }
            Err(err) => {
                self.record_error(frame, &mut data, &err);
                self.error_reply(frame, &err)
            }
        }
    }

    /// Queue the one `bindApplication` delivery, after `attachApplication`, then
    /// the one `scheduleTransaction` delivery once the guest pushed it (P4b).
    fn take_launch_delivery(&mut self) -> Option<Frame> {
        let wire = self.launch_app_thread?;
        if !self.launch_delivered {
            let bytes = self.launch_bind.clone()?;
            let seq = self.next_launch_seq();
            self.launch_delivered = true;
            eprintln!(
                "acos-binder-peer: delivering bindApplication ({} bytes, code {}) to java binder wire={wire}",
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
        if self.launch_txn_delivered {
            return None;
        }
        let bytes = self.launch_txn.clone()?;
        let seq = self.next_launch_seq();
        self.launch_txn_delivered = true;
        eprintln!(
            "acos-binder-peer: delivering ClientTransaction ({} bytes, \
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

    /// The `system -> app` frames queued by anything that ran on the host (a
    /// binder transaction, or a module's live `control` op). Public so the
    /// launcher can flush them after a control request, which is not followed
    /// by a binder transaction of its own.
    pub fn take_delivery_frames(&mut self) -> Vec<Frame> {
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
        if std::env::var_os("ACOS_SERVICE_CTL_LOG").is_some() && !pending.is_empty() {
            let path = std::env::var("ACOS_SERVICE_CTL_LOG").unwrap_or_default();
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                let _ = writeln!(
                    file,
                    "deliveries={} targets={:?}",
                    pending.len(),
                    pending.iter().map(|d| (d.interface.clone(), d.code, d.target.0)).collect::<Vec<_>>()
                );
            }
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
            let seq = self.next_txn_seq;
            self.next_txn_seq = self.next_txn_seq.wrapping_add(1).max(1);
            let mut frame = frame_from_parcel(F_TXN, &Frame { seq, ..Default::default() }, &d.data);
            frame.target = u64::from(wire);
            frame.code = d.code;
            frame.flags = 0;
            out.push(frame);
        }
        out
    }
}

impl Default for Peer {
    fn default() -> Self {
        Self::new()
    }
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
    let mut p = import_parcel_raw(frame);
    let d = p.peek_interface_token().ok().flatten();
    (d, p.data_size())
}

/// Import a frame's parcel into an `acos-parcel` `Parcel`.
///
/// Binder objects are rewritten from the driver's wire form to the peer's
/// `BinderId` form (`JAVA_TAG`/`NODE_TAG`), and `BINDER_TYPE_FD` objects get the
/// descriptor that arrived in `SCM_RIGHTS` (one per object, in order).
#[must_use]
pub fn import_parcel(frame: &Frame) -> Parcel {
    import_parcel_raw(frame)
}

fn import_parcel_raw(frame: &Frame) -> Parcel {
    // libbinder writes a 4-byte stability record right after every
    // `flat_binder_object` (`Parcel::unflattenBinder` reads it back). The
    // in-process `acos-parcel` format does not carry it, so strip it here and
    // rebase the object offsets. Without this every binder reads as null and
    // the following argument is misaligned.
    let mut out = Vec::with_capacity(frame.data.len());
    let mut object_offsets: Vec<usize> = Vec::new();
    let mut cursor = 0usize;
    let mut fds = frame.fds.iter().copied();
    // Offsets of the rewritten flat objects in `out`: the imported parcel has no
    // object table (`unmarshall` does not rebuild one), and both the reply
    // marshaller and the `writeValue` length accounting need it.
    let mut objects: Vec<usize> = Vec::new();
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
                    JAVA_TAG | (id & MASK32)
                }
            }
            BINDER_TYPE_FD => fds.next().map_or(id, |fd| fd as u64),
            _ => id,
        };
        let mut obj = [0u8; 24];
        obj.copy_from_slice(&frame.data[off..off + 24]);
        obj[8..16].copy_from_slice(&new_id.to_le_bytes());
        // The object table travels out of band in the frame, so the imported
        // parcel must be told where each rewritten object landed: a handler that
        // looks for a client binder in the request (`result_receiver` in the
        // bluetooth module) walks `Parcel::objects()`. Before this the imported
        // parcel answered zero objects for every guest request. MODNOTIF5 also
        // needs them for `writeValue` length accounting (a framework length
        // prefix still counts the 4-byte stability records import strips).
        object_offsets.push(out.len());
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
    parcel.unmarshall(&out);
    for offset in object_offsets {
        parcel.register_object(offset);
    }
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
