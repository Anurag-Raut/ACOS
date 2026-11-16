//! The relay's control surface, as the module framework sees it.
//!
//! The code lives in `acos-netd-ctl` (it needs `libc`, and this crate is
//! `#![forbid(unsafe_code)]`); this module is the one path `acos-services` code
//! uses, so the module's control ops and `acos_net` cannot drift apart.
//!
//! * [`client`] — the relay supervisor's client (`status`, `offline`, `delay`,
//!   `host_port`, …).
//! * [`net_ctl`] — the `acos_net` action names, schema and parser.
//! * [`proto`] — the line protocol and its structured error codes.

pub use acos_netd_ctl::{client, net_ctl, proto, NETD_SOCK};
