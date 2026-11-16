//! Host side of the ACOS WebView engine (ADR-0031).
//!
//! The guest gets a small, framed protocol on a Unix socket; behind it lives a
//! pinned `chrome-headless-shell` in its own bwrap sandbox, one process per
//! host, one browser context per session and one target per WebView.
//!
//! ```text
//! guest provider (WV2)                host broker (this crate)
//!   android.webkit.WebView   ──wire──▶  server::Conn  ──CDP pipe──▶  engine::EngineCache
//!        WIRE.md v1                     view::View                   sandbox::bwrap_argv
//! ```
//!
//! Layers, top to bottom:
//! * [`gate`] — the per-session lazy listener that hands the first guest
//!   connection to the shared broker (one engine per host, nothing before);
//! * [`server`] — the session socket(s), request dispatch, per-view event pumps;
//! * [`view`] — one target: navigation, JS, frames, input, interception;
//! * [`ax`] — the accessibility tree in display coordinates;
//! * [`broker`] — the engine supervisor (lazy start, crash restart, idle stop);
//! * [`engine`] — the pinned download + checksum + install;
//! * [`sandbox`] — the bwrap argv the engine runs under;
//! * [`cdp`] — the DevTools client over `--remote-debugging-pipe` (no TCP);
//! * [`wire`] — the framed guest protocol, normative copy of `WIRE.md`;
//! * [`error`] — the one error type and its stable wire codes.

pub mod ax;
pub mod broker;
pub mod cdp;
pub mod engine;
pub mod error;
pub mod gate;
pub mod sandbox;
pub mod server;
pub mod view;
pub mod wire;

pub use broker::{Broker, BrokerConfig, EngineVersion, Session};
pub use cdp::{CdpClient, CdpError};
pub use engine::{EngineCache, ENGINE_URL, ENGINE_VERSION, ENGINE_ZIP_SHA256};
pub use error::{Error, Result};
pub use gate::{BrokerSource, Gate, GateConfig, GateStats};
pub use server::{serve, serve_shared, ServerConfig, ServerHandle, SharedConfig};
pub use view::{View, Viewport};
pub use wire::{Blob, ErrorBody, Frame, WireError, PROTO};

/// The engine's identity as a verdict field (ADR-0031 item 3: "verdicts name the
/// engine version and `fidelity` states it is host Blink").
pub fn fidelity_note(version: &EngineVersion) -> serde_json::Value {
    serde_json::json!({
        "engine": version.product,
        "engineVersion": version.version,
        "revision": version.revision,
        "fidelity": "host Blink (Chrome for Testing chrome-headless-shell), not the phone's \
                     Android System WebView build",
    })
}
