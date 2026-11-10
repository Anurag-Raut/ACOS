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
//! The runtime is behind the [`backend::Backend`] trait. The only implementation
//! today is [`backend::CliBackend`], which executes the `acos` binary as a
//! subprocess for the commands that binary actually implements and returns a
//! structured `unsupported` envelope for every command that does not exist yet.
//! As runtime capabilities land in `acos-cli`, they become available tools with
//! no server changes.

#![deny(unsafe_op_in_unsafe_fn)]
// One controlled `libc::kill` call (see `process::kill_tree`) is the only unsafe
// code in this crate; everything else is safe Rust.

pub mod backend;
pub mod base64;
pub mod descriptors;
pub mod gate;
pub mod help;
pub mod jsonrpc;
pub mod process;
pub mod safety;
pub mod server;
pub mod session;
pub mod transport;

pub use backend::{Backend, BackendConfig, BackendInfo, CallContext, Capabilities, CliBackend};
pub use descriptors::{
    LATEST_PROTOCOL_VERSION, SERVER_NAME, SERVER_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};
pub use process::CancelToken;
pub use server::{Server, ServerConfig};
