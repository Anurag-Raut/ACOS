//! The relay's **control surface**: the supervisor's wire protocol, the client
//! that speaks it, and the `acos_net` verb's action table.
//!
//! These lived inside `acos-netd` (the supervisor binary) until NET5. They are
//! the *module's* control path — `network`'s [`Module::control`] ops are exactly
//! the `acos_net` actions — so the module framework has to be able to use them.
//! The dependency has to point `acos-services` → this crate and
//! `acos-netd` → `acos-services`, so the control surface is its own leaf crate:
//! it needs `libc` for `SCM_RIGHTS`, `setsid` and `kill`, and
//! `acos-services` is `#![forbid(unsafe_code)]`.
//!
//! * [`proto`] — one JSON object per line over the control socket.
//! * [`client`] — find (or start) the host's supervisor, then speak that
//!   protocol: `status`, `spawn`, `detach`, `offline`, `delay`, `host_port`.
//! * [`net_ctl`] — the `acos_net` action names, their closed JSON Schema and the
//!   parser; `acos-mcp` advertises the schema verbatim and both surfaces read it.
//!
//! [`Module::control`]: https://docs.rs/acos-services

pub mod client;
pub mod net_ctl;
pub mod proto;

/// The name of the supervisor's control socket inside its state directory.
pub const NETD_SOCK: &str = "netd.sock";
