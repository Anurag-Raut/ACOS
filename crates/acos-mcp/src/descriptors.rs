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

/// Server-local in-process session tool names (served by
/// [`crate::session_backend::SessionBackend`], not by the `acos` subprocess).
///
/// `acos_doctor` is also server-local: it delegates to `acos doctor --json`,
/// but its report is a JSON object rather than an ACOS envelope, so it is
/// handled in the same dispatch path.
pub const SESSION_TOOL_NAMES: &[&str] = &[
    "acos_launch",
    "acos_observe_tree",
    "acos_tap",
    "acos_key",
    "acos_text",
    "acos_swipe",
    "acos_logs",
    "acos_status",
    "acos_doctor",
];

/// Identify a command from its wire name (`run-dex`).
pub fn command_from_wire(wire: &str) -> Option<CommandName> {
    all_commands().iter().copied().find(|c| c.as_str() == wire)
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
            None => format!(
                "\n\n[MCP: {availability} fidelity={}]",
                self.fidelity.as_str()
            ),
        };
        let description = format!("{}{marker}", self.description);

        let read_only = matches!(
            self.command,
            Some(
                CommandName::Inspect
                    | CommandName::Observe
                    | CommandName::Events
                    | CommandName::Profile
                    | CommandName::CapabilityReport
            )
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
    tools.extend(session_tools());
    tools
}

/// The tools `tools/list` advertises: every tool this build can actually run.
///
/// An unavailable protocol command (`acos_capture`, `acos_assert`,
/// `acos_events`, `acos_profile`, `acos_capability_report`, ...) is **hidden**
/// rather than listed and answered with `unsupported`: a fresh agent that sees
/// 23 tools tries the stubs, pays a round trip for each and learns nothing. The
/// names stay reserved — [`tool_infos`] (and `docs/MCP.md`) still carry them, the
/// protocol's `mcp_tools()` descriptors are unchanged, and a call to a hidden
/// name returns the same structured `unsupported` envelope it always did.
#[must_use]
pub fn available_tool_infos(caps: &Capabilities) -> Vec<ToolInfo> {
    tool_infos(caps)
        .into_iter()
        .filter(|tool| tool.available)
        .collect()
}

/// The server-local in-process session tools.
///
/// They are marked available unconditionally: the default `acos-mcp` binary
/// installs the [`crate::session_backend::SessionBackend`] which serves them.
/// A server wired to the bare [`crate::backend::CliBackend`] answers with a
/// structured `unsupported` envelope rather than a false pass.
pub fn session_tools() -> Vec<ToolInfo> {
    let mut out = Vec::with_capacity(SESSION_TOOL_NAMES.len());
    for name in SESSION_TOOL_NAMES {
        let (description, schema) = session_tool_spec(name);
        // `acos_doctor` is a host preflight, not a runtime session op — and, like
        // `acos_help`/`acos_capabilities`, it says nothing about app behaviour, so
        // its fidelity is `n/a` rather than a misleading `static-analysis`.
        let fidelity = if *name == "acos_doctor" {
            Fidelity::NotApplicable
        } else {
            Fidelity::NativeRuntime
        };
        out.push(ToolInfo {
            tool_name: (*name).to_string(),
            command: None,
            description: description.to_string(),
            input_schema: schema,
            available: true,
            fidelity,
            reason: None,
        });
    }
    out
}

/// The published argument schema of one tool, for the dispatcher's
/// unknown-argument check.
///
/// It is the same schema `tools/list` publishes (protocol command, session tool,
/// `acos_capabilities`, `acos_help`), so the rule a client validates against and
/// the rule the server enforces cannot drift.
#[must_use]
pub fn tool_schema(tool_name: &str) -> Option<Value> {
    if tool_name == CAPABILITIES_TOOL {
        return Some(capabilities_tool().input_schema);
    }
    if tool_name == HELP_TOOL {
        return Some(help_tool().input_schema);
    }
    if SESSION_TOOL_NAMES.contains(&tool_name) {
        return Some(session_tool_spec(tool_name).1);
    }
    // ARGS: the protocol descriptors already close their object
    // (`McpToolDescriptor::new`), so the schema a client validates and the
    // schema this check enforces are the same document.
    acos_protocol::mcp_tools()
        .tools
        .into_iter()
        .find(|tool| tool.name == tool_name)
        .map(|tool| tool.input_schema)
}

/// The tool names `tools/list` may advertise: protocol commands plus
/// `acos_capabilities`, `acos_help` and the session tools.
#[must_use]
pub fn known_tool_names() -> Vec<String> {
    let mut names: Vec<String> = acos_protocol::mcp_tools()
        .tools
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    names.push(CAPABILITIES_TOOL.to_string());
    names.push(HELP_TOOL.to_string());
    names.extend(SESSION_TOOL_NAMES.iter().map(|name| (*name).to_string()));
    names
}

fn session_tool_spec(name: &str) -> (&'static str, Value) {
    let base = json!({
        "type": "object",
        "description": "Identity fields; only `session_id` is meaningful here.",
        "properties": {
            "request_id": {"type": "string"},
            "session_id": {"type": "string"}
        }
    });
    let with_base = |properties: Value| {
        // ARGS: a closed object. An unknown argument is a caller error, not a
        // flag to ignore: `acos_observe_tree {"screenshot": true}` on a build
        // without it read as a success (feedback 2026-11-15). The server
        // enforces the same rule in `crate::args::check`, so a client that does
        // not validate still cannot get a silent no-op.
        let mut schema = json!({
            "type": "object",
            "properties": properties,
            "additionalProperties": false
        });
        schema["properties"]["base"] = base.clone();
        schema
    };
    match name {
        "acos_launch" => (
            "Call this FIRST for an app you want to drive: it boots the APK in an in-process \
             session that persists across MCP calls (the CLI relaunches per invocation). Returns \
             the honest launch stage ladder (apk_parsed -> process_booted -> bind_application -> \
             application_on_create -> client_transaction -> activity_on_create -> on_resume -> \
             first_frame) plus `session_id`, `reached_stage` and `classes_loaded`. `ok:true` is \
             only returned when every rung ran; otherwise the first blocked rung carries the \
             structured blocker with a code and `details`. Re-launching the same session id \
             replaces the previous task. Common errors: APK_*, WRONG_ABI, CONFINEMENT_UNAVAILABLE \
             (native), TIMEOUT, RESOURCE_LIMIT, UNSUPPORTED_JAVA_API/UNSUPPORTED_ANDROID_API (a \
             framework native the engine does not implement yet, naming `details.api`).",
            with_base(json!({
                "apk": {"type": "string", "description": "Path to the .apk, under ACOS_ALLOWED_ROOTS."},
                "timeout_ms": {"type": "integer", "minimum": 1, "description": "Wall-clock ceiling for the launch (defaults to the server call budget)."},
                "display": {
                    "type": "string",
                    "description": "Display override for the in-process session, `WIDTHxHEIGHT@DPI` (for \
                                    example `1080x2400@400`). This argument is accepted by the \
                                    in-process engine; the ART daemon route reports the display it \
                                    used instead of overriding it."
                },
                "settle": {
                    "type": "string", "enum": ["fast", "strict", "content"],
                    "description": "How the launch reply binds its tree. `strict` (default) keeps the \
                                    150 ms quiet window and is assertion evidence. `fast` binds the app's \
                                    first traversal from a fresh in-guest probe sample with no quiet \
                                    window; it may be mid-animation and is never assertion evidence. \
                                    `content` (SE2) is the content-stable rule for a screen that animates \
                                    forever: settled when the UI tree content signature is unchanged \
                                    for a >=300 ms window across >=2 traversals; it backs UI/text \
                                    observations only, never pixels. The reply's `settle` object names \
                                    the mode and how it was bound."
                },
                "pixels": {
                    "type": "boolean",
                    "description": "Capture a PNG for the launch reply (default false). Without it the \
                                    reply carries the bound C1 frame identity (`buffer_generation`, \
                                    `frame_number`) and no md5; `settle:fast` and `settle:content`                                     refuse pixels (a content-settled frame is tree-content evidence                                     only; its pixels may still be animating)."
                },
                "settle_timeout_ms": {
                    "type": "integer", "minimum": 1, "maximum": 600000,
                    "description": "P12: bound the strict/fast settle wait. On timeout the reply still \
                                    carries the latest tree and frame identity with `settled:false` and \
                                    `bound:timeout`; a timeout is never assertion evidence."
                },
                "animations": {
                    "type": "string", "enum": ["on", "off"],
                    "description": "P12: per-session framework animation scales. `on` (default) is real \
                                    Android behaviour. `off` sets the three animation scales to 0 \
                                    (standard CI practice) so functional checks are not slowed by the \
                                    framework's ~0.9 s press/elevation animator; never use it for \
                                    animation/transition testing. Launch-time only: every reply states \
                                    `animations`."
                },
                "native": {
                    "type": "boolean",
                    "description": "Run the APK's lib/<abi>/*.so under the ADR-0022 confinement. \
                                    The multi-threaded MCP server re-execs a fresh, \
                                    single-threaded child session process that enters the \
                                    sandbox before launching; later observe/action calls are \
                                    proxied to that confined child."
                },
                "label": {
                    "type": "string",
                    "description": "FB5: a name for this session, echoed in the launch reply and in \
                                    `acos_status` (`sessions[].label`). Give each session you park a \
                                    label you can recognise — the server destroys its own idle \
                                    sessions, and the label is how you tell which one went."
                },
                "idle_ttl_s": {
                    "type": "integer", "minimum": 0,
                    "description": "FB5: seconds of idleness before this server destroys the session \
                                    (default 600 = 10 min, or ACOS_SESSION_IDLE_TTL_S; `0` disables). \
                                    Any later MCP call on the session renews the clock. An expired \
                                    session's guest is gone and the next call returns \
                                    SESSION_NOT_FOUND with `details.reason=idle_ttl_expired` and the \
                                    TTL, so a parked session fails loudly instead of eating memory."
                }
            })),
        ),
        "acos_observe_tree" => (
            "Walk the live framework View hierarchy of the held session (WindowManagerGlobal -> \
             ViewRootImpl -> ViewGroup) and return the structured node tree (class, bounds, text, \
             resource_id, clickable/enabled/focused/scrollable, children). Call it after \
             `acos_launch`, before any selector-based tap. Before the app adds a window it returns \
             `UNSUPPORTED_BACKEND` with `details.reason=not_ready` and `details.stage_reached`, \
             never an empty tree. Common errors: UNSUPPORTED_BACKEND (not_ready), TIMEOUT.",
            with_base(json!({
                "max_nodes": {"type": "integer", "minimum": 1, "description": "Truncate the walk after this many nodes."},
                "settle": {
                    "type": "string", "enum": ["fast", "strict", "content"],
                    "description": "`strict` (default) is the event-settled rule and assertion evidence. \
                                    `fast` binds the newest probe sample immediately (may be \
                                    mid-animation; not assertion evidence). `content` (SE2) proves \
                                    the tree content stable for a >=300 ms window and is the \
                                    documented answer for a screen that animates forever in pixels."
                },
                "settle_timeout_ms": {
                    "type": "integer", "minimum": 1, "maximum": 600000,
                    "description": "P12: bound the strict settle wait. On timeout the tree is still \
                                    returned with `settled:false` and `bound:timeout`."
                },
                "screenshot": {
                    "type": ["boolean", "string"],
                    "description": "FB2: `true` writes this frame's lossless PNG under the session's \
                                    artifacts and returns `screenshot.path` (absolute), `width`, \
                                    `height`, `display_size`, `display_origin` and `frame_seq`, plus \
                                    a bounded inline preview in this reply. A string writes to that \
                                    path instead. A placeholder/no-pixel frame answers \
                                    NO_FRESH_FRAME, never an empty success. `display_origin` is the \
                                    offset between PNG pixels and tree bounds: png_y = display_y - \
                                    display_origin.y."
                }
            })),
        ),
        "acos_tap" => (
            "Tap in the held session. Identify the target by `id` (a node whose `resource_id` \
             equals it, or its bare local name after `/`, exactly what `acos_observe_tree` \
             exposes), by `content_desc` (a node's accessibility label — what a React Native \
             pressable carries its whole name in, e.g. \"Search Japanese dictionary\"), by \
             `text` (the platform's *accessible name*: the node's text, else its content \
             description among nodes the user can see), by a React Native `testID` (resolves \
             to the nearest clickable ancestor), or by a point `x`,`y` (both required). \
             Precedence is `id`, then `content_desc`, then `text`, then `testID`, then `x`/`y`. \
             A selector that matches more than one user-visible node is refused as \
             `AMBIGUOUS_SELECTOR` with every candidate (`id`, `text`, `content_desc`, `bounds`) \
             — narrow it or pass `index`; nothing is tapped silently. Requires \
             `acos_observe_tree` first for selector taps. Returns action/accepted/input_ack_seq \
             plus `changes`, the settled tree diff of the tap (`added`/`removed`/`changed` \
             nodes, current activity, focused node) so the next step usually needs no \
             `acos_observe_tree`; `diff:false` turns it off. \
             Common errors: AMBIGUOUS_SELECTOR (several matches), ELEMENT_NOT_FOUND with \
             `details.kind=NoSuchNode`/`NoUiTree` (observe first), ELEMENT_OFFSCREEN (only an \
             invisible node matches), TIMEOUT. A React Native session has no View tree and no \
             pixel frame, so point taps are unsupported there.",
            with_base(json!({
                "diff": {
                    "type": "boolean",
                    "description": "FB3: include `changes`, the settled UI-tree diff of this action \
                                    (added/removed/changed nodes with `id`/`text`/`content_desc`/ \
                                    `class`/`bounds`; a changed node also lists which `fields` \
                                    changed, old -> new; plus the current activity and the focused \
                                    node). Computed from trees the session already holds, so it \
                                    costs no extra traversal and no extra guest round trip. \
                                    Default `true`; `false` returns the pre-FB3 reply byte for \
                                    byte. Each list is capped at 40 entries with \
                                    `truncated:true` and the true `counts`; \
                                    `changes.baseline:\"none\"` means the session held no \
                                    pre-action tree (no diff is invented)."
                },
                "x": {"type": "integer", "description": "X coordinate for a point tap (requires `y`)."},
                "y": {"type": "integer", "description": "Y coordinate for a point tap (requires `x`)."},
                "text": {"type": "string", "description": "Node label to tap: the platform's accessible name (text, else content description) among visible nodes."},
                "content_desc": {"type": "string", "description": "Node `content_desc` to tap (exact). Prefer it for React Native `accessibilityLabel`s; takes precedence over `text`."},
                "id": {"type": "string", "description": "Node `resource_id` (or bare local name) to tap; takes precedence over content_desc/text and x/y."},
                "index": {"type": "integer", "minimum": 0, "description": "Pick the Nth match of the other fields, document order. Use it only when the same label is legitimately repeated."},
                "testID": {"type": "string", "description": "React Native testID to tap (resolves to the nearest clickable ancestor); used when `id` and `text` are absent."},
                "settle": {
                    "type": "string", "enum": ["fast", "strict", "content"],
                    "description": "`fast` for navigation (may be mid-animation); `strict` (default) is \
                                    the event-settled rule; `content` (SE2) is the content-stable rule \
                                    for a screen that animates forever in pixels (tree content proven \
                                    stable >=300 ms; never pixel evidence)."
                },
                "settle_timeout_ms": {
                    "type": "integer", "minimum": 1, "maximum": 600000,
                    "description": "P12: bound the settle wait. On timeout the reply still carries the \
                                    latest tree and frame identity with `settled:false`, `bound:timeout`."
                },
                "expect_change": {
                    "type": "boolean",
                    "description": "SE1: opt-in verification of the action's effect. After the \
                                    strict settle, fail the action (ASSERTION_FAILED, \
                                    `details.reason=\"expect_change\"`, `pixels_changed:false`, \
                                    before/after frame refs) when the settled frame's pixels are \
                                    identical to the pre-action frame. Use it on a tap/type/key \
                                    whose point is to change the screen; `settle:fast` with \
                                    `expect_change:true` is rejected (a fast frame is never \
                                    assertion evidence), and a reply with no measured \
                                    `pixels_changed` is UNSUPPORTED_ASSERTION, never a pass."
                },
            })),
        ),
        "acos_key" => (
            "Press a key in the held session. `code` is an Android key code (4=BACK, 66=ENTER, \
             3=HOME) or a name (`back`, `home`, `enter`, `menu`, `tab`, `escape`, `delete`). \
             Returns `changes` (the settled tree diff of the key) by default. \
             Common errors: ELEMENT_NOT_FOUND with `details.kind=NoFocusedWindow` (launch/observe first).",
            with_base(json!({
                "diff": {
                    "type": "boolean",
                    "description": "FB3: include `changes`, the settled UI-tree diff of this action \
                                    (added/removed/changed nodes with `id`/`text`/`content_desc`/ \
                                    `class`/`bounds`; a changed node also lists which `fields` \
                                    changed, old -> new; plus the current activity and the focused \
                                    node). Computed from trees the session already holds, so it \
                                    costs no extra traversal and no extra guest round trip. \
                                    Default `true`; `false` returns the pre-FB3 reply byte for \
                                    byte. Each list is capped at 40 entries with \
                                    `truncated:true` and the true `counts`; \
                                    `changes.baseline:\"none\"` means the session held no \
                                    pre-action tree (no diff is invented)."
                },
                "code": {"type": ["integer", "string"], "description": "Numeric key code or name."},
                "expect_change": {
                    "type": "boolean",
                    "description": "SE1: opt-in verification of the action's effect. After the \
                                    strict settle, fail the action (ASSERTION_FAILED, \
                                    `details.reason=\"expect_change\"`, `pixels_changed:false`, \
                                    before/after frame refs) when the settled frame's pixels are \
                                    identical to the pre-action frame. Use it on a tap/type/key \
                                    whose point is to change the screen; `settle:fast` with \
                                    `expect_change:true` is rejected (a fast frame is never \
                                    assertion evidence), and a reply with no measured \
                                    `pixels_changed` is UNSUPPORTED_ASSERTION, never a pass."
                },
            })),
        ),
        "acos_text" => (
            "Type `text` into the session's focused editor — the same path a scenario `type` \
             step takes: the guest's own `type_text` command drives the focused text field \
             (real `InputConnection.commitText`/virtual-keyboard path), so `TextWatcher`s, the \
             selection and the cursor update as on a device. There is no need for the node's \
             class to be `EditText`: React Native `TextInput`s are obfuscated subclasses, so a \
             field you focused by `content_desc` accepts text. Focus the field first (observe, \
             then `acos_tap`), then type. The reply's `result.target.node` is the field the input went to, \
             as the tree shows it (the focused node). The typed text is echoed only for a focused non-password field; \
             otherwise it is masked (`target.text_masked = true`, `chars` only, `reason`): a \
             password field, `mask: true`, or an unknown target (the tree names no focused field \
             -- nothing is guessed). Common errors: UNSUPPORTED_BACKEND when a character cannot be \
             represented on the key-event fallback, TIMEOUT when the screen never settles. Composing text and IME \
             actions are not modelled. Returns `changes` (the settled tree diff of the typing) \
             by default; `diff:false` turns it off.",
            with_base(json!({
                "diff": {
                    "type": "boolean",
                    "description": "FB3: include `changes`, the settled UI-tree diff of this action \
                                    (added/removed/changed nodes with `id`/`text`/`content_desc`/ \
                                    `class`/`bounds`; a changed node also lists which `fields` \
                                    changed, old -> new; plus the current activity and the focused \
                                    node). Computed from trees the session already holds, so it \
                                    costs no extra traversal and no extra guest round trip. \
                                    Default `true`; `false` returns the pre-FB3 reply byte for \
                                    byte. Each list is capped at 40 entries with \
                                    `truncated:true` and the true `counts`; \
                                    `changes.baseline:\"none\"` means the session held no \
                                    pre-action tree (no diff is invented)."
                },
                "text": {"type": "string", "description": "Text to type into the focused field."},
                "mask": {"type": "boolean", "description": "Withhold the typed text from the reply (`target.text_masked`, `target.chars`). Use it for a secret whose field the tree does not report as focused. Without it, a focused non-password field is echoed and any other target is masked."},
                "expect_change": {
                    "type": "boolean",
                    "description": "SE1: opt-in verification of the action's effect. After the \
                                    strict settle, fail the action (ASSERTION_FAILED, \
                                    `details.reason=\"expect_change\"`, `pixels_changed:false`, \
                                    before/after frame refs) when the settled frame's pixels are \
                                    identical to the pre-action frame. Use it on a tap/type/key \
                                    whose point is to change the screen; `settle:fast` with \
                                    `expect_change:true` is rejected (a fast frame is never \
                                    assertion evidence), and a reply with no measured \
                                    `pixels_changed` is UNSUPPORTED_ASSERTION, never a pass."
                },
            })),
        ),
        "acos_swipe" => (
            "Swipe in the held session from `from` to `to`; each point is an object with integer \
             `x` and `y`. `duration_ms` defaults to 300. The ART engine has no swipe command \
             (UNSUPPORTED_BACKEND, `input.swipe`), so it returns no `changes` there. `changes` \
             (the `diff` argument) applies when the engine supports the action. Common errors: ELEMENT_NOT_FOUND with \
             `details.kind=NoWindowAtPoint` (no frame yet), INVALID_ARGUMENT (a point missing x/y).",
            with_base(json!({
                "diff": {
                    "type": "boolean",
                    "description": "FB3: include `changes`, the settled UI-tree diff of this action \
                                    (added/removed/changed nodes with `id`/`text`/`content_desc`/ \
                                    `class`/`bounds`; a changed node also lists which `fields` \
                                    changed, old -> new; plus the current activity and the focused \
                                    node). Computed from trees the session already holds, so it \
                                    costs no extra traversal and no extra guest round trip. \
                                    Default `true`; `false` returns the pre-FB3 reply byte for \
                                    byte. Each list is capped at 40 entries with \
                                    `truncated:true` and the true `counts`; \
                                    `changes.baseline:\"none\"` means the session held no \
                                    pre-action tree (no diff is invented)."
                },
                "from": {"type": "object", "properties": {"x": {"type": "integer"}, "y": {"type": "integer"}}, "required": ["x", "y"]},
                "to": {"type": "object", "properties": {"x": {"type": "integer"}, "y": {"type": "integer"}}, "required": ["x", "y"]},
                "duration_ms": {"type": "integer", "minimum": 1}
            })),
        ),
        "acos_logs" => (
            "Read the guest log buffer of the held session. By DEFAULT it returns the agent-relevant \
             view: records at priority `W` and above plus anything that names the app's own package, \
             so framework/SoLoader noise does not bury the app's words. Optional `tag` (substring, \
             case-insensitive), `min_priority` (`V`/`D`/`I`/`W`/`E`/`F` or `2`..=`7`, default `W`) and \
             `all: true` (no defaults at all: the whole buffer) filter the records. The unfiltered \
             buffer stays on disk (`logs/logcat.txt` in a verify run), and `min_priority: \"V\"` \
             returns it. Consecutive identical records are collapsed: each carries `repeat_count` and \
             its `line` gains a `(repeated N times)` suffix. Returns `total`, `count`, `collapsed`, \
             `filter` (the defaults that were applied) plus `launched` and `stage_reached`. An empty \
             buffer is a valid pass, but is only meaningful once the app reached `onCreate`; check \
             `stage_reached` before concluding the run was clean. It never fabricates lifecycle lines.",
            with_base(json!({
                "tag": {"type": "string", "description": "Keep only records whose tag contains this substring."},
                "min_priority": {"type": ["string", "integer"], "description": "Minimum logcat priority (`V`/`D`/`I`/`W`/`E`/`F` or 2..7). Defaults to `W`."},
                "all": {"type": "boolean", "description": "Return the whole buffer with no default priority/package filter (the same view as `min_priority: \"V\"`)."}
            })),
        ),
        "acos_status" => (
            "Report whether the held session has launched, the last launch stage reached, and the \
             published frame count. Use it to tell 'app never started' from 'app started but no \
             window/frame yet' before reading logs or observing. Also lists **this server's** \
             sessions under `sessions[]` (FB5): `session_id`, `label`, `package`, `activity`, \
             `age_s`, `idle_s`, the applicable `idle_ttl_s`, the guest's `guest_pid` and \
             `guest_rss_kb`, and `live`. Use it to find and clean up every session you left \
             behind — the server also reclaims its own after the idle TTL.",
            with_base(json!({})),
        ),
        "acos_doctor" => (
            "Run the host preflight (`acos doctor`) and return its JSON report. Call it FIRST when \
             a build or launch fails before the app runs: it checks the Android SDK/build-tools, \
             `android.jar`, the JDK (`java`/`javac`/`keytool`), the workspace helper and debug \
             keystore, the boot-classpath cache, and host memory. Each failing check carries a \
             concrete `fix` string. The tool returns `fail` with `ASSERTION_FAILED` when any \
             required check fails, with the full report under `result.checks`.",
            with_base(json!({})),
        ),
        _ => ("In-process session tool.", with_base(json!({}))),
    }
}

/// Render the full tool list as a deterministic Markdown table.
///
/// This is the single source of the `docs/MCP.md` tool table: the
/// `docs_tool_table_matches_descriptors` test compares the checked-in block
/// against this output, so the docs cannot drift from the descriptor list.
/// Availability and per-call schemas are still delivered live by `tools/list`;
/// only the structural list is generated here.
#[must_use]
pub fn tools_markdown() -> String {
    let caps = Capabilities::unprobed();
    let mut out = String::from("| tool | kind | command | fidelity | required args |\n");
    out.push_str("|---|---|---|---|---|\n");
    for tool in tool_infos(&caps) {
        let kind = if tool.command.is_some() {
            "protocol"
        } else {
            "server"
        };
        let command = tool
            .command
            .map(|command| format!("`{}`", command.as_str()))
            .unwrap_or_else(|| "-".to_string());
        let required: Vec<String> = tool
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|name| format!("`{name}`"))
                    .collect()
            })
            .unwrap_or_default();
        let required = if required.is_empty() {
            "-".to_string()
        } else {
            required.join(", ")
        };
        out.push_str(&format!(
            "| `{}` | {kind} | {command} | `{}` | {required} |\n",
            tool.tool_name,
            tool.fidelity.as_str()
        ));
    }
    out
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
        fidelity: Fidelity::NotApplicable,
        reason: None,
    }
}

/// The `acos_help` tool descriptor.
pub fn help_tool() -> ToolInfo {
    ToolInfo {
        tool_name: HELP_TOOL.to_string(),
        command: None,
        description: concat!(
            "The ACOS agent guide. With no `topic` this returns a SHORT skeleton: the three status ",
            "rules, the loop in ten lines, and the topic list with one-line descriptions. Call it ",
            "before your first ACOS call, then ask for one section by name: `mcp` (every advertised ",
            "MCP tool: required/optional args, one example call, what it returns — generated from the ",
            "tool descriptors), `workflow` (the loop plus the screenshot/type/content_desc/prove-a-flow ",
            "recipes), `cli` (every command with a runnable example), `scenarios` (writing a verify YAML), ",
            "`errors` (code -> action), `limits` (one support row per route for native Views, React ",
            "Native and Flutter; API 34, x86_64, rotation, memory admission). `topic: \"all\"` returns the ",
            "complete guide; an unknown topic returns the skeleton again."
        )
        .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "enum": ["overview", "mcp", "workflow", "cli", "scenarios", "errors", "limits", "all"],
                    "description": "Optional section of the guide: `overview` (the default skeleton), \n`workflow` (the loop), `cli` (commands + examples), `scenarios` (verify YAML), `errors` \n(codes -> action), `limits` (what is not supported), `all` (the whole guide)."
                }
            },
            "additionalProperties": false
        }),
        available: true,
        fidelity: Fidelity::NotApplicable,
        reason: None,
    }
}

/// Instructions returned by `initialize` (and embedded in `acos_help`).
pub const INSTRUCTIONS: &str = concat!(
    "ACOS exposes an agent-oriented Android verification API. ",
    "Start with acos_help (the full agent guide: loop, CLI examples, scenario YAML, error playbook, limits) ",
    "and acos_capabilities for what is implemented. ",
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
        // 13 protocol tools + acos_capabilities + acos_help + 9 session tools.
        assert_eq!(infos.len(), 24);
        let names: BTreeSet<&str> = infos.iter().map(|t| t.tool_name.as_str()).collect();
        for descriptor in mcp_tools().tools {
            assert!(
                names.contains(descriptor.name.as_str()),
                "{}",
                descriptor.name
            );
        }
        for info in &infos {
            let is_server_tool = info.tool_name == CAPABILITIES_TOOL
                || info.tool_name == HELP_TOOL
                || SESSION_TOOL_NAMES.contains(&info.tool_name.as_str());
            if !is_server_tool {
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
        assert!(json["description"]
            .as_str()
            .unwrap()
            .contains("available=false"));
        assert_eq!(json["title"], "acos run");
    }
}
