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

pub mod a11y_bind; // MODA11Y2: system -> app AccessibilityService bind
pub mod a11y_config; // MODA11Y6: <accessibility-service> meta-data -> capabilities
pub mod ams;
pub mod apk_facts; // PERF-REL3b: one APK open per launch facts set (manifest, budget)
pub mod animation;
pub mod atms;
pub mod clipboard;
pub mod codec;
pub mod connectivity; // NET1: IConnectivityManager default network + callbacks
pub mod core_module; // ADR-0029: today's registrations as the always-attached `core` module
pub mod compositor_client;
pub mod cursor;
pub mod declared;
pub mod display;
pub mod display_events;
pub mod error;
pub mod exception;
pub mod extras;
pub mod host;
pub mod ime;
pub mod ime_wire;
pub mod input;
pub mod input_channel;
pub mod keychars; // W16: stock KeyCharacterMap (KCM) -> native parcel
pub mod manifest_resolve; // W27: MAIN/LAUNCHER resolution incl. <activity-alias>
pub mod misc;
pub mod module;
pub mod netd; // the relay's control surface: protocol, client, `acos_net`'s schema
pub mod modules; // ADR-0029: the shipped non-core modules
pub mod network; // ADR-0029/NET5: which network profile this session attached
pub mod native_libs; // W17: APK x86_64 libs -> per-run nativeLibraryDir
pub mod parcelable;
pub mod pending_intent; // MODNOTIF4: PendingIntent sender records + the IIntentSender nodes
pub mod permissions; // MODNOTIF2: the one runtime-permission table
pub mod pms;
pub mod provider;
pub mod refs; // BINDERREF2: the guest-binder registration ledger (host strong refs)
pub mod registry;
pub mod sensors; // MODSENS: native sensorservice (SensorManager/SensorEventConnection)
pub mod surface_client;
pub mod trace;
pub mod value;
pub mod window_session;
pub mod wms;

pub use error::{codes, Result, ServiceError};
pub use module::{
    Module, ModuleConfig, ModuleManifest, ModuleRegistry, ModuleReport, ModuleSpec, Profile,
};
pub use host::{CallResult, PendingDelivery, ServiceHost, ServiceObject, Wiring};
pub use parcelable::{FieldOp, ParcelableSpec, Verification};
pub use value::{Bundle, Parcelable, Value};

/// Resolve a repo-relative path from the install root, the CWD or the crate
/// manifest dir.
///
/// Tests run with the crate directory as their CWD, so the fixture/DEX paths in
/// the repo root are found by walking ancestors. An installed ACOS resolves them
/// against `$ACOS_HOME` first (REL1a). The runtime never depends on this: in
/// production the native layer passes explicit paths.
#[must_use]
pub fn workspace_path(rel: &str) -> Option<std::path::PathBuf> {
    let mut bases = Vec::new();
    if let Some(home) = std::env::var_os("ACOS_HOME") {
        if !home.is_empty() {
            let home = std::path::PathBuf::from(home);
            let candidate = home.join(rel);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
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
