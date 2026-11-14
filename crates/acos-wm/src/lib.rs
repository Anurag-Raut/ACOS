//! # acos-wm — the ACOS window manager, compositor, input dispatcher, IME and
//! vsync generator
//!
//! ACOS replaces `system_server`'s WindowManagerService, SurfaceFlinger, the
//! input dispatcher and the IME service with **in-process Rust state machines**
//! that are unit-testable without the VM. This crate is that state machine: it
//! consumes the real AIDL wire layouts (`acos-aidl`), speaks the real Android
//! `Parcel` format (`acos-parcel`), hands the final frame to the software
//! renderer (`acos-render`) and describes frames/events with the agent protocol
//! (`acos-protocol`).
//!
//! ## Event-driven, never polling
//!
//! Nothing here owns a thread or a sleep. The vsync generator computes a
//! deadline only while a client has an outstanding `scheduleVsync`; with no
//! request [`vsync::VsyncGenerator::next_deadline`] returns `None` and the
//! runtime's `epoll_wait(-1)` blocks forever. [`clock::VirtualClock`] makes that
//! observable in tests: every timer arm goes through the clock, so a test can
//! assert "zero wakeups while idle".
//!
//! ## Layout
//!
//! ```text
//! display    the real 1080x2400@420 display geometry and system-bar insets
//! codec      typed Parcel codecs for the WMS parcelables (order from the DEX)
//! wms        IWindowSession/IWindowManager window state machine
//! callbacks  the outgoing IWindow callback queue (resized/insetsControlChanged/…)
//! compositor SurfaceControl layer tree + acos-render presentation
//! input      InputDispatcher/InputChannel: routing, injection, ack accounting
//! ime        IInputMethodManager + IRemoteInputConnection minimal behaviour
//! vsync      DisplayEventReceiver/Choreographer vsync generator
//! facade     SessionWm: the one object the native layer and the agent API use
//! replay     run the measured binder_calls_{launch,interaction}.tsv through it
//! ```
//!
//! The crate has no `unsafe` at all: even the socketpair/timerfd plumbing uses
//! the safe `rustix` wrappers, so `#![forbid(unsafe_code)]` holds everywhere.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod callbacks;
pub mod clock;
pub mod codec;
pub mod compositor;
pub mod display;
pub mod error;
pub mod facade;
pub mod geom;
pub mod ime;
pub mod input;
pub mod replay;
pub mod transport;
pub mod vsync;
pub mod wms;

pub use clock::{Clock, RealClock, VirtualClock};
pub use display::{DisplaySpec, RotationRelaunch, SystemBars};
pub use acos_protocol::Orientation;
pub use error::{Result, WmError};
pub use callbacks::{PendingCallback, WmCallback};
pub use facade::{FrameView, InputOutcome, SessionWm};
pub use geom::{InsetsI, PointI, RectI};
pub use wms::{AddWindowResult, RelayoutResult, WindowRecord, WindowState};

/// Version of the derived WMS layouts, surfaced in evidence metadata.
pub const ACOS_WM_VERSION: &str = env!("CARGO_PKG_VERSION");
