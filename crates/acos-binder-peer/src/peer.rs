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

use crate::protocol::{Frame, F_REP, F_REQ, F_RSP, F_TXN, PING_TRANSACTION, TF_ONE_WAY};

/// Bit 32 marks a driver wire handle (a Java local binder).
pub const JAVA_TAG: u64 = 1 << 32;
/// Bit 33 marks one of this peer's service nodes.
pub const NODE_TAG: u64 = 1 << 33;

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
    /// Reverse replies received from Java (peer-initiated transactions are
    /// two-way here, matching the runtime's `BinderDelivery`).
    pub reverse_replies: u64,
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer")
            .field("host", &self.host)
            .field("java_binders", &self.java_binders)
            .field("unsupported", &self.unsupported.len())
            .field("reverse_replies", &self.reverse_replies)
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
            reverse_replies: 0,
        }
    }

    /// The host (tests/inspection).
    #[must_use]
    pub fn host(&self) -> &ServiceHost {
        &self.host
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
            F_REP => self.reverse_replies += 1,
            other => {
                eprintln!("acos-binder-peer: ignoring frame type {other}");
            }
        }
        out.extend(self.take_delivery_frames());
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
            return Some(self.service_transaction(frame, frame.target & !NODE_TAG));
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

    fn service_manager(&mut self, frame: &Frame) -> Frame {
        // addService is the one IServiceManager call the service host cannot
        // answer: it registers a binder that lives in the *app* process.
        if frame.code == 3 {
            return self.add_service(frame);
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
            Ok(()) => frame_from_parcel(F_RSP, frame, &reply),
            Err(err) => {
                self.record_error(frame, &mut data, &err);
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
        let descriptor = data
            .peek_interface_token()
            .ok()
            .flatten()
            .unwrap_or_else(|| "<unknown>".into());
        let method = acos_aidl::lookup(&descriptor, frame.code)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| format!("code {}", frame.code));
        let record = UnsupportedRecord {
            descriptor,
            code: frame.code,
            method,
            error: err.code().to_string(),
            message: err.to_string(),
        };
        self.log_unsupported(&record);
    }

    fn log_unsupported(&mut self, record: &UnsupportedRecord) {
        let line = record.to_json();
        eprintln!("acos-binder-peer: UNSUPPORTED_ANDROID_API {line}");
        if let Some(file) = self.log.as_mut() {
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
        self.unsupported.push(record.clone());
    }

    fn take_delivery_frames(&mut self) -> Vec<Frame> {
        let mut pending = std::mem::take(&mut self.local_deliveries);
        pending.extend(self.host.take_deliveries());
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
                    JAVA_TAG | (id & MASK32)
                }
            }
            BINDER_TYPE_FD => fds.next().map_or(id, |fd| fd as u64),
            _ => id,
        };
        let mut obj = [0u8; 24];
        obj.copy_from_slice(&frame.data[off..off + 24]);
        obj[8..16].copy_from_slice(&new_id.to_le_bytes());
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
    parcel.set_data_position(0);
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
                fds.push(parsed.id as i32);
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
        offs.extend_from_slice(&(out.len() as u64).to_le_bytes());
        out.extend_from_slice(&obj);
        if add_stability {
            // Stability::Level::SYSTEM == 0b1100 (E3G §3.3).
            out.extend_from_slice(&12i32.to_le_bytes());
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
