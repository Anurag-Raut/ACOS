//! MCP protocol constants and tool-descriptor augmentation.
//!
//! The tool list is the protocol's [`acos_protocol::mcp_tools`] plus two server
//! tools (`acos_capabilities`, `acos_help`). Every command appears whether or not
//! it is implemented: the descriptor is annotated with `available` and `fidelity`
//! in `annotations` and `_meta` so an agent can see what exists *now*.

use acos_protocol::{mcp_tools, Backend, CommandName, Fidelity};
use serde_json::{json, Value};

use crate::backend::Capabilities;

/// The protocol revision this server defaults to.
pub const LATEST_PROTOCOL_VERSION: &str = "2025-03-26";

/// Protocol revisions this server understands, newest first.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26"];

/// Choose the revision to use for a client's requested version.
///
/// If the client requests a revision we implement we echo it; otherwise we
/// answer with our default, which the client must accept or disconnect.
pub fn negotiate(client_version: &str) -> String {
    if SUPPORTED_PROTOCOL_VERSIONS.contains(&client_version) {
        client_version.to_string()
    } else {
        LATEST_PROTOCOL_VERSION.to_string()
    }
}

/// Whether the negotiated revision carries `structuredContent` (2025-06-18).
pub fn supports_structured_content(version: &str) -> bool {
    version == "2025-06-18"
}

/// Server name reported in `initialize`.
pub const SERVER_NAME: &str = "acos-mcp";

/// Server version reported in `initialize`.
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Tool name for the capability probe.
pub const CAPABILITIES_TOOL: &str = "acos_capabilities";

/// Tool name for the self-description.
pub const HELP_TOOL: &str = "acos_help";

/// Identify a command from its wire name (`run-dex`).
pub fn command_from_wire(wire: &str) -> Option<CommandName> {
    all_commands()
        .iter()
        .copied()
        .find(|c| c.as_str() == wire)
}

/// Identify a command from its MCP tool name (`acos_run_dex`).
pub fn command_from_tool(tool: &str) -> Option<CommandName> {
    all_commands()
        .iter()
        .copied()
        .find(|c| c.tool_name() == tool)
}

/// Every command, in stable order.
pub fn all_commands() -> &'static [CommandName] {
    &[
        CommandName::Inspect,
        CommandName::RunDex,
        CommandName::Run,
        CommandName::Observe,
        CommandName::Capture,
        CommandName::Action,
        CommandName::Assert,
        CommandName::Events,
        CommandName::Reset,
        CommandName::Profile,
        CommandName::Destroy,
        CommandName::Verify,
        CommandName::CapabilityReport,
    ]
}

/// The fidelity a command produces (or will produce) when it works.
pub fn command_fidelity(command: CommandName) -> Fidelity {
    match command {
        CommandName::Inspect | CommandName::CapabilityReport => Fidelity::StaticAnalysis,
        _ => Fidelity::NativeRuntime,
    }
}

/// The backend expected to serve a command when implemented.
pub fn command_backend(_command: CommandName) -> Backend {
    Backend::AcosNative
}

/// One tool as exposed by this server.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    /// MCP tool name.
    pub tool_name: String,
    /// The underlying ACOS command, when this is a protocol tool.
    pub command: Option<CommandName>,
    /// Agent-facing description.
    pub description: String,
    /// JSON Schema for `arguments`.
    pub input_schema: Value,
    /// Whether the current backend can execute it.
    pub available: bool,
    /// Expected fidelity.
    pub fidelity: Fidelity,
    /// Why it is unavailable, when it is.
    pub reason: Option<String>,
}

impl ToolInfo {
    /// Render the tool for `tools/list`, annotated for the negotiated revision.
    pub fn to_json(&self, protocol_version: &str) -> Value {
        let availability = if self.available {
            "available=true".to_string()
        } else {
            "available=false".to_string()
        };
        let marker = match self.command {
            Some(command) => format!(
                "\n\n[MCP: {availability} fidelity={} command={}]",
                self.fidelity.as_str(),
                command.as_str()
            ),
            None => format!("\n\n[MCP: {availability} fidelity={}]", self.fidelity.as_str()),
        };
        let description = format!("{}{marker}", self.description);

        let read_only = matches!(
            self.command,
            Some(CommandName::Inspect | CommandName::Observe | CommandName::Events | CommandName::Profile | CommandName::CapabilityReport)
        ) || self.command.is_none();
        let annotations = json!({
            "title": title_for(self),
            "readOnlyHint": read_only,
            "destructiveHint": false,
            "idempotentHint": read_only,
            "openWorldHint": false,
            "available": self.available,
            "fidelity": self.fidelity.as_str(),
        });
        let meta = json!({
            "acos": {
                "available": self.available,
                "fidelity": self.fidelity.as_str(),
                "command": self.command.map(|c| c.as_str()),
                "tool": self.tool_name,
                "reason": self.reason,
            }
        });
        let mut tool = json!({
            "name": self.tool_name,
            "description": description,
            "inputSchema": self.input_schema,
            "annotations": annotations,
            "_meta": meta,
        });
        if protocol_version == "2025-06-18" {
            // `title` is a first-class field in 2025-06-18.
            tool["title"] = json!(title_for(self));
        }
        tool
    }
}

fn title_for(tool: &ToolInfo) -> String {
    match tool.command {
        Some(command) => format!("acos {}", command.as_str()),
        None => tool.tool_name.clone(),
    }
}

/// Build all tool descriptors for the current capability snapshot.
pub fn tool_infos(caps: &Capabilities) -> Vec<ToolInfo> {
    let mut tools: Vec<ToolInfo> = mcp_tools()
        .tools
        .into_iter()
        .map(|descriptor| {
            let command = command_from_tool(&descriptor.name);
            let command = command.or_else(|| {
                descriptor
                    .name
                    .strip_prefix("acos_")
                    .and_then(|s| command_from_wire(&s.replace('_', "-")))
            });
            let (available, reason) = match command {
                Some(command) if caps.is_supported(command) => (true, None),
                Some(_) => (
                    false,
                    Some(if caps.probed {
                        "the acos backend does not implement this command yet".to_string()
                    } else {
                        caps.probe_error
                            .clone()
                            .unwrap_or_else(|| "the acos backend was not found".to_string())
                    }),
                ),
                None => (false, Some("unrecognised protocol command".to_string())),
            };
            ToolInfo {
                tool_name: descriptor.name,
                command,
                description: descriptor.description,
                input_schema: descriptor.input_schema,
                available,
                fidelity: command
                    .map(command_fidelity)
                    .unwrap_or(Fidelity::StaticAnalysis),
                reason,
            }
        })
        .collect();
    tools.push(capabilities_tool());
    tools.push(help_tool());
    tools
}

/// The `acos_capabilities` tool descriptor.
pub fn capabilities_tool() -> ToolInfo {
    ToolInfo {
        tool_name: CAPABILITIES_TOOL.to_string(),
        command: None,
        description: concat!(
            "Report what the ACOS backend can do right now: which ACOS commands are implemented, ",
            "the resolved `acos` binary/version, and the server's limits. ",
            "Call it when a tool returns `unsupported` or when the available set may have changed; ",
            "pass `refresh: true` to re-probe the backend after a rebuild."
        )
        .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "refresh": {
                    "type": "boolean",
                    "description": "Re-run the backend capability probe before answering."
                }
            },
            "additionalProperties": false
        }),
        available: true,
        fidelity: Fidelity::StaticAnalysis,
        reason: None,
    }
}

/// The `acos_help` tool descriptor.
pub fn help_tool() -> ToolInfo {
    ToolInfo {
        tool_name: HELP_TOOL.to_string(),
        command: None,
        description: concat!(
            "Self-description for agents: the ACOS inspect -> verify -> read-verdict -> fix loop, ",
            "the error-code playbook, the currently supported vs unsupported commands, and the ",
            "fidelity rules. Start here when you are unsure which tool to call next."
        )
        .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "description": "Optional topic: `workflow`, `errors`, `fidelity` or `commands`."
                }
            },
            "additionalProperties": false
        }),
        available: true,
        fidelity: Fidelity::StaticAnalysis,
        reason: None,
    }
}

/// Instructions returned by `initialize` (and embedded in `acos_help`).
pub const INSTRUCTIONS: &str = concat!(
    "ACOS exposes an agent-oriented Android verification API. ",
    "Start with acos_help for the workflow and acos_capabilities for what is implemented. ",
    "The core loop is inspect -> verify -> read the verdict -> fix -> rerun. ",
    "`unsupported` means the engine cannot do it: never rewrite the app to hide it, never call it a pass. ",
    "`fail` means the app ran and a check did not hold: read the assertion diff. ",
    "Tool results are the ACOS JSON envelope; `isError` is true whenever status != pass."
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn command_lookup_round_trips() {
        for command in all_commands() {
            assert_eq!(command_from_wire(command.as_str()), Some(*command));
            assert_eq!(command_from_tool(command.tool_name()), Some(*command));
        }
        assert_eq!(command_from_tool("nope"), None);
    }

    #[test]
    fn every_protocol_tool_is_mapped_to_a_command() {
        let caps = Capabilities::unprobed();
        let infos = tool_infos(&caps);
        // 13 protocol tools + acos_capabilities + acos_help.
        assert_eq!(infos.len(), 15);
        let names: BTreeSet<&str> = infos.iter().map(|t| t.tool_name.as_str()).collect();
        for descriptor in mcp_tools().tools {
            assert!(names.contains(descriptor.name.as_str()), "{}", descriptor.name);
        }
        for info in &infos {
            if info.tool_name != CAPABILITIES_TOOL && info.tool_name != HELP_TOOL {
                assert!(info.command.is_some(), "{}", info.tool_name);
            }
        }
    }

    #[test]
    fn availability_reflects_capabilities() {
        let mut caps = Capabilities::unprobed();
        caps.supported.insert(CommandName::Inspect);
        let infos = tool_infos(&caps);
        let inspect = infos
            .iter()
            .find(|t| t.tool_name == "acos_inspect")
            .unwrap();
        assert!(inspect.available);
        let run = infos.iter().find(|t| t.tool_name == "acos_run").unwrap();
        assert!(!run.available);
        let json = run.to_json("2025-06-18");
        assert_eq!(json["annotations"]["available"], false);
        assert_eq!(json["_meta"]["acos"]["available"], false);
        assert!(json["description"].as_str().unwrap().contains("available=false"));
        assert_eq!(json["title"], "acos run");
    }
}
