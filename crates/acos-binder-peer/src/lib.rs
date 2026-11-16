//! `acos-binder-peer` — the real ACOS `ServiceHost` behind the userspace binder
//! driver from E3G.
//!
//! The driver (`native/binder_driver.c`, bionic `LD_PRELOAD`) makes stock
//! `libbinder` — `android.os.Binder`, `BinderProxy`, `Parcel`, `ServiceManager`
//! — work with no `/dev/binder`. This crate is the other end of its socket:
//!
//! ```text
//! stock libbinder -> LD_PRELOAD driver -> unix socket -> Peer -> ServiceHost
//! ```
//!
//! * service-manager lookups resolve against the host's real service table;
//! * transactions are forwarded to [`acos_services::ServiceHost::transact`];
//! * `system -> app` deliveries become `BR_TRANSACTION` to Java local binders;
//! * an unknown `(descriptor, code)` is a structured
//!   `UNSUPPORTED_ANDROID_API` record, never a fake success.
//!
//! Run the binary with a socket path, e.g.
//! `acos-binder-peer /data/acos-binder.sock`, and point the driver at it with
//! `E3G_SOCK=/data/acos-binder.sock` (`ACOS_BINDER_SOCK` is equivalent).

// The transport module needs `sendmsg`/`recvmsg` for SCM_RIGHTS; everything
// else in the crate is safe Rust.
#![warn(missing_docs)]

pub mod peer;
pub mod protocol;
pub mod serve;

pub use peer::{
    frame_from_parcel, host_binder_kind, import_parcel, import_parcel_for, java_binder_id, java_wire,
    owner_process, peer_node, ForeignNode, Peer, ProcessConn, TagRefusal, UnsupportedRecord,
    FOREIGN_FLAG, JAVA_TAG, MAX_FOREIGN_NODES, NODE_TAG, PROC_MASK, PROC_SHIFT,
};
pub use serve::{serve, refuse_frame, ServeOutcome};
pub use protocol::{
    accept, close_frame_fds, listen, read_frame, write_frame, Frame, F_DEATH, F_HELLO, F_REP, F_REQ,
    F_RSP, F_TXN, RSP_DEAD, MAX_PROCESSES, MAX_PROCESS_TAG, PING_TRANSACTION, TAG_PRIMARY, TF_ONE_WAY,
    TF_STATUS_CODE,
};
