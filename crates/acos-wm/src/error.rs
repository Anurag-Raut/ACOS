//! Structured errors for the window-manager state machines.

use thiserror::Error;

/// Convenience alias.
pub type Result<T> = std::result::Result<T, WmError>;

/// Every failure the WMS/compositor/input/ime/vsync layer can report.
#[derive(Debug, Error)]
pub enum WmError {
    /// A `Parcel` operation failed (underflow, bad string, …).
    #[error("parcel: {0}")]
    Parcel(#[from] acos_parcel::ParcelError),

    /// A parcelable's bytes did not follow the derived framework layout.
    #[error("codec {what}: {reason}")]
    Codec {
        /// Parcelable being decoded.
        what: &'static str,
        /// Why decoding failed.
        reason: String,
    },

    /// The requested window token is unknown to this session.
    #[error("unknown window {0}")]
    UnknownWindow(u64),

    /// The requested session id is unknown.
    #[error("unknown session {0}")]
    UnknownSession(u64),

    /// A transaction code that the derived AIDL table does not contain.
    #[error("no AIDL method {descriptor}#{code}")]
    UnknownTransaction {
        /// Interface descriptor.
        descriptor: String,
        /// Transaction code.
        code: u32,
    },

    /// Noise: no `IWindow` callback target is registered for the window.
    #[error("window {0} has no callback binder")]
    NoCallback(u64),

    /// The IME is not bound (a declared V1 divergence, not a crash).
    #[error("ime not bound: {0}")]
    ImeNotBound(String),

    /// An operation was attempted on a released `SurfaceControl`.
    #[error("released SurfaceControl {0}")]
    ReleasedSurface(u64),

    /// The requested display does not exist.
    #[error("unknown display {0}")]
    UnknownDisplay(i32),

    /// The input sequence id does not match any outstanding event.
    #[error("input ack for unknown sequence {0}")]
    UnknownInputSequence(u64),

    /// A socketpair/eventfd/timerfd syscall failed.
    #[error("fd: {0}")]
    Fd(String),

    /// The renderer rejected a frame (e.g. `NO_FRESH_FRAME`).
    #[error("render: {0}")]
    Render(#[from] acos_render::RenderError),

    /// A feature is deliberately not implemented in V1.
    #[error("unsupported: {0}")]
    Unsupported(String),
}
