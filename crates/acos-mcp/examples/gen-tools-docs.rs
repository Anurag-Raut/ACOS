//! Print the generated `docs/MCP.md` tool table.
//!
//! Usage:
//!   cargo run -p acos-mcp --example gen-tools-docs
//!
//! The output is the exact block that `tools/docs.rs` checks between the
//! `ACOS_TOOLS_BEGIN` / `ACOS_TOOLS_END` markers.

fn main() {
    print!("{}", acos_mcp::descriptors::tools_markdown());
}
