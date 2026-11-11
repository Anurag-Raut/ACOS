//! # acos-services — the in-process Android system services
//!
//! ACOS runs the real Android 14 framework bytecode on a Rust VM. The framework's
//! AIDL proxies write real `Parcel` bytes through a single native
//! (`BinderProxy.transactNative`); this crate is the other end. It answers those
//! transactions **in process** with pure-Rust service state machines, so it has
//! no dependency on the VM: it only sees typed values and `Parcel` buffers, and
//! is therefore unit-testable today.
//!
//! The later native layer calls:
//!
//! ```text
//! ServiceHost::transact(binder_handle, code, &mut data, &mut reply, flags) -> Result<()>
//! ```
//!
//! ## Design
//!
//! * [`host::ServiceHost`] owns the [`acos_parcel::BinderRegistry`] and the
//!   service objects. It dispatches on the **interface token** in the request
//!   parcel and looks up `(descriptor, code)` in the tables decoded by
//!   [`acos_aidl`] from the real boot DEX — there is no hand-typed code table.
//! * [`codec`] reads the arguments in the mechanically-decoded
//!   [`acos_aidl::WireOp`] order, so a handler cannot write fields out of order.
//! * [`parcelable`] holds the field-order codecs, transcribed from the
//!   framework's own `writeToParcel` bytecode (see its `scan` oracle).
//! * Services are constructed **lazily** on first `checkService`/`transact`, so
//!   merely hosting the launch set costs almost nothing.
//! * Unknown `(descriptor, code)` pairs are a structured
//!   [`error::codes::UNSUPPORTED_ANDROID_API`], never a default success.
//! * A per-call trace ([`trace`]) feeds the agent evidence stream; it is off by
//!   default.
//!
//! ## Scope
//!
//! This crate owns `registry`, `pms`, `atms`, `provider`, `display` and the
//! small services. `IWindowManager`/`IWindowSession`/`IWindow` and the IME
//! interfaces are owned by the concurrent `acos-wm` agent and are reported as
//! [`registry::owner_of`] (`acos-wm`), never faked.

#![forbid(unsafe_code)]

pub mod ams;
pub mod atms;
pub mod clipboard;
pub mod declared;
pub mod codec;
pub mod cursor;
pub mod display;
pub mod error;
pub mod exception;
pub mod host;
pub mod ime;
pub mod input;
pub mod misc;
pub mod parcelable;
pub mod pms;
pub mod provider;
pub mod registry;
pub mod trace;
pub mod value;
pub mod wms;

pub use error::{codes, Result, ServiceError};
pub use host::{CallResult, PendingDelivery, ServiceHost, ServiceObject, Wiring};
pub use parcelable::{FieldOp, ParcelableSpec, Verification};
pub use value::{Bundle, Parcelable, Value};

/// Resolve a repo-relative path from the CWD or the crate manifest dir.
///
/// Tests run with the crate directory as their CWD, so the fixture/DEX paths in
/// the repo root are found by walking ancestors. The runtime never depends on
/// this: in production the native layer passes explicit paths.
#[must_use]
pub fn workspace_path(rel: &str) -> Option<std::path::PathBuf> {
    let mut bases = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        bases.push(cwd);
    }
    if let Some(dir) = option_env!("CARGO_MANIFEST_DIR") {
        bases.push(std::path::PathBuf::from(dir));
    }
    for base in bases {
        for ancestor in base.ancestors() {
            let candidate = ancestor.join(rel);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}
