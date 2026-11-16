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

pub use peer::{
    frame_from_parcel, host_binder_kind, import_parcel, java_wire, peer_node, wait_for_idle, IdleWake,
    Peer, UnsupportedRecord, JAVA_TAG, NODE_TAG,
};
pub use protocol::{
    accept, close_frame_fds, listen, read_frame, write_frame, Frame, F_HELLO, F_REF, F_REP, F_REQ,
    F_RSP, F_TXN, PING_TRANSACTION, PROTOCOL_VERSION, REF_ACQUIRE, REF_RELEASE, TF_ONE_WAY,
    TF_STATUS_CODE,
};
