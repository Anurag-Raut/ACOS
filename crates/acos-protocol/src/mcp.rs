//! MCP `tools/list` descriptors, as data.
//!
//! There is deliberately no MCP server in this crate. The CLI/service can adapt
//! [`mcp_tools`] directly; the descriptions are written for an AI agent and say
//! when to use a tool, what it returns, and the errors that matter.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::{
    ActionRequest, AssertRequest, CapabilityReportRequest, CaptureRequest, CommandName,
    DestroyRequest, EventsRequest, InspectRequest, ObserveRequest, ProfileRequest, ResetRequest,
    RunDexRequest, RunRequest, VerifyRequest,
};
use crate::schema::schema_value;

/// One MCP tool descriptor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpToolDescriptor {
    /// Tool name (`acos_<command>`).
    pub name: String,
    /// Agent-facing description: when to use, what it returns, common errors.
    pub description: String,
    /// JSON Schema for the request object.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

impl McpToolDescriptor {
    /// Build a descriptor from a request schema.
    pub fn new<T: JsonSchema>(
        command: CommandName,
        description: impl Into<String>,
    ) -> McpToolDescriptor {
        // ARGS: the published schema is a **closed** object. schemars does not
        // emit `additionalProperties: false` for a struct, and an open schema
        // tells a client that any extra key is acceptable — which is how
        // `acos_observe_tree {"screenshot": true}` on a build that ignored it
        // looked like a success (feedback 2026-11-15). The server rejects an
        // unknown argument for the same reason; see `acos-mcp::args`.
        let mut input_schema = schema_value::<T>();
        if let Some(object) = input_schema.as_object_mut() {
            object.insert("additionalProperties".to_string(), Value::Bool(false));
        }
        McpToolDescriptor {
            name: command.tool_name().to_string(),
            description: description.into(),
            input_schema,
        }
    }
}

/// The payload returned by MCP `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpToolsList {
    /// All tools, in stable order.
    pub tools: Vec<McpToolDescriptor>,
}

/// Build the MCP `tools/list` payload.
pub fn mcp_tools() -> McpToolsList {
    McpToolsList {
        tools: vec![
            McpToolDescriptor::new::<InspectRequest>(
                CommandName::Inspect,
                concat!(
                    "Use FIRST for any APK you have not seen: static analysis only (no execution). ",
                    "Returns package, version, min/target SDK, launcher activity, permissions, components, ",
                    "DEX/native/asset inventory, resources and signature-scheme detection. ",
                    "Never treat this as proof of runtime behaviour. ",
                    "Common errors: APK_NOT_ZIP, APK_NO_MANIFEST, AXML_MALFORMED, ARSC_MALFORMED."
                ),
            ),
            McpToolDescriptor::new::<RunDexRequest>(
                CommandName::RunDex,
                concat!(
                    "Execute a DEX `main` on the ACOS Rust VM (console tier), not an Android app. ",
                    "Returns exit_code, stdout, stderr and any uncaught exception. ",
                    "Use for dexprog/unit-level verification; Android API use will return UNSUPPORTED_JAVA_API. ",
                    "Common errors: DEX_BAD_*, DEX_VERIFY_FAILED, UNSUPPORTED_BYTECODE, VM_INTERNAL."
                ),
            ),
            McpToolDescriptor::new::<RunRequest>(
                CommandName::Run,
                concat!(
                    "NOT THE CANONICAL TOOL — prefer `acos_launch`, which boots the APK on real ART in a ",
                    "session that persists across MCP calls and returns the launch stage ladder. This tool ",
                    "keeps the reference/generic session path (its own isolation/display vocabulary) and is ",
                    "kept for compatibility. ",
                    "Create a session by launching an APK activity. Every later observe/capture/action/assert targets the returned session_id. ",
                    "Set isolation and display explicitly: `sandbox` for independent app data, `none` display for headless logic. ",
                    "Returns session_id, package, effective isolation/display/fidelity and optionally the first frame. ",
                    "Common errors: UNSUPPORTED_ABI, ISOLATION_UNAVAILABLE, NEEDS_REAL_DEVICE, REQUIRES_GMS, APK_*."
                ),
            ),
            McpToolDescriptor::new::<ObserveRequest>(
                CommandName::Observe,
                concat!(
                    "NOT THE CANONICAL TOOL — prefer `acos_observe_tree` for the live View hierarchy ",
                    "(nodes, `content_desc`, bounds, flags) of an ART session; that is what selectors act on. ",
                    "This tool observes the reference session (tree and/or text plus frame metadata) and is ",
                    "kept for compatibility. ",
                    "Read the current UI tree and/or text for a session. Selectors in later steps refer to this tree's nodes. ",
                    "Returns node id/class/resource_id/text/content_desc/bounds/flags and frame metadata (frame_seq, cause_id, content_hash, meaningful). ",
                    "A headless session returns NO_DISPLAY; a route without text returns NO_TEXT. ",
                    "Common errors: SESSION_NOT_FOUND, NO_DISPLAY, NO_TEXT, NO_FRESH_FRAME."
                ),
            ),
            McpToolDescriptor::new::<CaptureRequest>(
                CommandName::Capture,
                concat!(
                    "Capture a FRESH frame (PNG or raw) for a session. Set `fresh: true` and use after_seq/after_cause after an action; ",
                    "a stale frame must never be reused. Returns frame metadata plus the written path. ",
                    "Common errors: NO_FRESH_FRAME (wait/re-observe), NO_DISPLAY, SESSION_NOT_FOUND."
                ),
            ),
            McpToolDescriptor::new::<ActionRequest>(
                CommandName::Action,
                concat!(
                    "NOT THE CANONICAL TOOL — prefer the one-purpose session tools on a live ART session: ",
                    "`acos_tap` (id/text/testID or a point), `acos_text` (focused editor), `acos_key` and ",
                    "`acos_swipe`. They report the action's own ack and settle evidence, including ",
                    "`handled` (the app window's own `finishInputEvent` answer) and `target` (the resolved ",
                    "node and the hit-test at the injected point). This tool keeps the ",
                    "generic multi-action path (tap, long_press, swipe, type, key, back, rotate, wait) and is ",
                    "kept for compatibility. ",
                    "Deliver one input action. Prefer selectors over coordinates. Returns accepted/state_changed/handled, an input-ack event seq and optionally a fresh frame. ",
                    "Common errors: ELEMENT_NOT_FOUND, SESSION_NOT_FOUND, TIMEOUT, APP_NOT_RESPONDING, NO_DISPLAY."
                ),
            ),
            McpToolDescriptor::new::<AssertRequest>(
                CommandName::Assert,
                concat!(
                    "Evaluate one assertion (text, visible, not_visible, state, log regex sequence, no_crash, screenshot) and return a machine pass/fail with a diff. ",
                    "`passed:false` means the assertion ran and did not hold; it is not an engine error. ",
                    "Common errors: ASSERTION_FAILED, ELEMENT_NOT_FOUND, NO_TEXT, NO_FRESH_FRAME."
                ),
            ),
            McpToolDescriptor::new::<EventsRequest>(
                CommandName::Events,
                concat!(
                    "Read the append-only session event stream since a cursor (lifecycle, log, crash, anr, frame, input-ack). ",
                    "Returns events and next_seq; pass next_seq as `since_seq` on the next call. ",
                    "Common errors: SESSION_NOT_FOUND, TIMEOUT (no matching event in the window)."
                ),
            ),
            McpToolDescriptor::new::<ResetRequest>(
                CommandName::Reset,
                concat!(
                    "Reset task/app/data/all state for a session and report what was actually reset. ",
                    "`state_reset:false` is possible for shared-task isolation (data is shared). ",
                    "Common errors: SESSION_NOT_FOUND, ISOLATION_UNSATISFIED."
                ),
            ),
            McpToolDescriptor::new::<ProfileRequest>(
                CommandName::Profile,
                concat!(
                    "Collect memory/CPU/frame metrics for a session. Returns a ResourceReport with explicitly named metrics. ",
                    "Use it to check budgets, not as a correctness verdict. ",
                    "Common errors: SESSION_NOT_FOUND, RESOURCE_LIMIT, TIMEOUT."
                ),
            ),
            McpToolDescriptor::new::<DestroyRequest>(
                CommandName::Destroy,
                concat!(
                    "Tear down a session and release its worker/display. Idempotent: destroying an absent session reports already_absent. ",
                    "After destroy, the session_id is invalid for every other command."
                ),
            ),
            McpToolDescriptor::new::<VerifyRequest>(
                CommandName::Verify,
                concat!(
                    "Run an APK + YAML scenario and write a complete artifacts directory (run.json, verdict.json, steps/, frames/, tree/, events.jsonl, resources.json, repro.*). ",
                    "This is the primary verification loop: inspect -> verify -> read verdict.json -> fix -> rerun. ",
                    "A pass is valid only if its evidence exists; `unsupported` is never a pass. Set required_fidelity when the task needs ART/device evidence. ",
                    "A run owns its artifacts directory: a non-empty `artifacts` directory written by another run is refused with INVALID_ARGUMENT naming that run (details.previous_run_id) unless `overwrite: true`. ",
                    "Common errors: INVALID_ARGUMENT, INVALID_SCENARIO, ARTIFACTS_INVALID, ASSERTION_FAILED, APP_CRASHED, UNSUPPORTED_*, UNVERIFIED_FIDELITY."
                ),
            ),
            McpToolDescriptor::new::<CapabilityReportRequest>(
                CommandName::CapabilityReport,
                concat!(
                    "Ask what the engine can actually do for an APK/scenario before spending time on verification. ",
                    "Returns one CapabilityCheck per required capability with supported/unsupported and a code/hint for gaps. ",
                    "`all_required_supported:false` means do not claim a pass; choose another backend/target or report unsupported."
                ),
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_tool_per_command_with_schema() {
        let tools = mcp_tools();
        assert_eq!(tools.tools.len(), 13);
        for tool in &tools.tools {
            assert!(tool.name.starts_with("acos_"), "{}", tool.name);
            assert!(tool.description.len() > 80, "{}", tool.name);
            assert!(
                tool.input_schema.get("properties").is_some(),
                "{}",
                tool.name
            );
            assert!(tool.input_schema.get("title").is_some(), "{}", tool.name);
        }
        let value = serde_json::to_value(&tools).unwrap();
        assert_eq!(value["tools"][0]["name"], "acos_inspect");
        assert!(value["tools"][0].get("inputSchema").is_some());
    }
}
