//! # acos-mcp
//!
//! An MCP (Model Context Protocol) stdio server that exposes the ACOS agent
//! API ([`docs/AGENT_API.md`](../../../docs/AGENT_API.md)) to AI coding agents.
//!
//! The server speaks newline-delimited JSON-RPC 2.0 on stdin/stdout (the MCP
//! stdio transport). stdout carries protocol frames only; every diagnostic goes
//! to stderr. Tools are the descriptors in [`acos_protocol::mcp_tools`], one per
//! ACOS command, plus two server tools: `acos_capabilities` and `acos_help`.
//!
//! The runtime is behind the [`backend::Backend`] trait. [`backend::CliBackend`]
//! executes the `acos` binary as a subprocess for the commands that binary
//! implements. [`session_backend::SessionBackend`] (installed by default) wraps
//! it with an in-process map of [`acos_runtime::session::AgentSession`]s so that
//! `acos_run`/`acos_launch`, `acos_observe`, `acos_action`/`acos_tap`/`acos_key`/
//! `acos_swipe`, `acos_logs`, `acos_status`, `acos_reset` and `acos_destroy`
//! hold their state across tool calls. As runtime capabilities land in
//! `acos-cli`, they become available tools with no server changes.

#![deny(unsafe_op_in_unsafe_fn)]
// One controlled `libc::kill` call (see `process::kill_tree`) is the only unsafe
// code in this crate; everything else is safe Rust.

pub mod backend;
pub mod base64;
pub mod daemon;
pub mod descriptors;
pub mod flutter_backend;
pub mod gate;
pub mod help;
pub mod jsonrpc;
pub mod process;
pub mod rn_backend;
pub mod safety;
pub mod server;
pub mod session;
pub mod session_backend;
pub mod transport;

pub use backend::{Backend, BackendConfig, BackendInfo, CallContext, Capabilities, CliBackend};
pub use session_backend::SessionBackend;
pub use descriptors::{
    LATEST_PROTOCOL_VERSION, SERVER_NAME, SERVER_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};
pub use process::CancelToken;
pub use server::{Server, ServerConfig};
