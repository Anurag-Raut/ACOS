//! The backend abstraction and the CLI-backed implementation.
//!
//! [`Backend`] is the only thing the server knows about execution. The
//! [`CliBackend`] runs the `acos` binary as a subprocess for commands the binary
//! implements, and returns a structured `unsupported` envelope (status
//! `unsupported`, `ok: false`, with a hint) for every command it does not.
//! Runtime capability is discovered by probing `acos --help`; nothing is
//! hard-coded beyond the fidelity each command is expected to produce.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use acos_protocol::{
    Backend as ProtocolBackend, CommandName, Envelope, ErrorCode, Fidelity, Metrics,
    ProtocolError, ResourceBudget,
};
use serde_json::{json, Value};

use crate::descriptors::{command_backend, command_fidelity};
use crate::process::{self, CancelToken, Captured, ChildSpec, RunError};
use crate::safety;

/// Default per-call wall-clock budget when the request declares none.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Default maximum captured bytes for stdout and stderr separately.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// Default capability-probe timeout.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Identity of the resolved backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    /// Resolved `acos` binary, when one was found.
    pub binary: Option<PathBuf>,
    /// `acos --version` output, when available.
    pub version: Option<String>,
}

/// A snapshot of what the backend can do.
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// Resolved binary, when found.
    pub binary: Option<PathBuf>,
    /// Version string, when probed.
    pub version: Option<String>,
    /// Whether a probe has run.
    pub probed: bool,
    /// Commands the backend reports it implements.
    pub supported: HashSet<CommandName>,
    /// Why probing failed, when it did.
    pub probe_error: Option<String>,
}

impl Capabilities {
    /// A snapshot before any probe: everything unavailable.
    pub fn unprobed() -> Self {
        Capabilities {
            binary: None,
            version: None,
            probed: false,
            supported: HashSet::new(),
            probe_error: Some("the backend has not been probed yet".to_string()),
        }
    }

    /// Whether a command is currently executable.
    pub fn is_supported(&self, command: CommandName) -> bool {
        self.supported.contains(&command)
    }

    /// JSON form for the `acos_capabilities` tool.
    pub fn to_json(&self) -> Value {
        let commands: Vec<Value> = crate::descriptors::all_commands()
            .iter()
            .map(|command| {
                let available = self.is_supported(*command);
                json!({
                    "command": command.as_str(),
                    "tool": command.tool_name(),
                    "available": available,
                    "fidelity": command_fidelity(*command).as_str(),
                    "reason": if available { Value::Null } else {
                        json!(self.probe_error.clone().unwrap_or_else(|| "not implemented by the acos backend yet".to_string()))
                    },
                })
            })
            .collect();
        json!({
            "binary": self.binary.as_ref().map(|p| p.display().to_string()),
            "version": self.version,
            "probed": self.probed,
            "probe_error": self.probe_error,
            "commands": commands,
        })
    }
}

/// Everything a backend needs for one call.
#[derive(Debug, Clone)]
pub struct CallContext {
    /// Cancellation flag, set by `notifications/cancelled`.
    pub cancel: CancelToken,
    /// Hard wall-clock deadline for the call.
    pub deadline: Instant,
    /// Declared resource budget from `base.budget`.
    pub budget: ResourceBudget,
    /// Canonical allowed path roots.
    pub allowed_roots: Vec<PathBuf>,
    /// Artifacts directory (also an allowed root).
    pub artifacts_dir: PathBuf,
    /// Maximum characters per string argument.
    pub max_arg_chars: usize,
    /// Maximum captured bytes per stream.
    pub max_output_bytes: usize,
}

/// The execution backend behind the MCP server.
pub trait Backend: Send + Sync + 'static {
    /// Execute one ACOS command and return its envelope.
    fn call(&self, command: CommandName, args: &Value, ctx: &CallContext) -> Envelope;
    /// The cached capability snapshot.
    fn capabilities(&self) -> Capabilities;
    /// Re-probe the backend and update the cache.
    fn refresh_capabilities(&self) -> Capabilities;
    /// Backend identity.
    fn info(&self) -> BackendInfo;
    /// Handle an in-process session tool (`acos_launch`, `acos_logs`, ...).
    ///
    /// The default returns `None`, which the server maps to a structured
    /// `unsupported` envelope. Only a backend that owns a live session (the
    /// [`crate::session_backend::SessionBackend`]) implements this.
    fn session_call(&self, _name: &str, _args: &Value, _ctx: &CallContext) -> Option<Envelope> {
        None
    }
    /// Destroy the sessions this process created (SERVELEAK).
    ///
    /// Called once when the transport ends (stdin EOF on stdio), so a client
    /// that exits cannot leave the backend holding a live guest it will never
    /// address again. A backend that holds no sessions keeps the default no-op;
    /// sessions a *different* process created (a shared daemon's) are never
    /// touched.
    fn shutdown(&self) {}
}

/// A shared backend is a backend: `main` holds the `Arc` for its SIGTERM
/// handler while the server serves from the same instance (FB5).
impl<T: Backend + ?Sized> Backend for std::sync::Arc<T> {
    fn call(&self, command: CommandName, args: &Value, ctx: &CallContext) -> Envelope {
        (**self).call(command, args, ctx)
    }

    fn capabilities(&self) -> Capabilities {
        (**self).capabilities()
    }

    fn refresh_capabilities(&self) -> Capabilities {
        (**self).refresh_capabilities()
    }

    fn info(&self) -> BackendInfo {
        (**self).info()
    }

    fn session_call(&self, name: &str, args: &Value, ctx: &CallContext) -> Option<Envelope> {
        (**self).session_call(name, args, ctx)
    }

    fn shutdown(&self) {
        (**self).shutdown();
    }
}

/// Configuration for [`CliBackend`].
#[derive(Debug, Clone)]
pub struct BackendConfig {
    /// Explicit `acos` binary; `None` triggers discovery.
    pub binary: Option<PathBuf>,
    /// Extra environment for every child.
    pub extra_env: Vec<(String, String)>,
    /// Capability-probe timeout.
    pub probe_timeout: Duration,
    /// Maximum captured bytes per stream during probing.
    pub max_output_bytes: usize,
}

impl Default for BackendConfig {
    fn default() -> Self {
        BackendConfig {
            binary: None,
            extra_env: Vec::new(),
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

impl BackendConfig {
    /// Configuration for an explicit binary (used by tests).
    pub fn for_binary(binary: impl Into<PathBuf>) -> Self {
        BackendConfig {
            binary: Some(binary.into()),
            ..BackendConfig::default()
        }
    }

    /// Discover the binary: `ACOS_BIN`, then `$ACOS_HOME/bin/acos`, then the
    /// `acos` next to this executable, then `acos` on `PATH`, then `target/release/acos` walking up from the cwd/executable.
    pub fn discover() -> Self {
        BackendConfig {
            binary: discover_binary(),
            ..BackendConfig::default()
        }
    }
}

/// Find the `acos` binary.
///
/// Order (FB2c, matching the v0.2.1 integration branch's `discover_binary` so the
/// later merge is trivial): `ACOS_BIN`, then `$ACOS_HOME/bin/acos` (an explicit
/// `ACOS_HOME` wins over an installed sibling: it is the operator's statement of
/// which CLI to drive), then the CLI installed **next to this executable** (the
/// REL1a layout: `<prefix>/bin/acos` beside `<prefix>/bin/acos-mcp`), then `acos`
/// on `PATH`, then `target/{release,debug}/acos` walking up from the cwd.
///
/// The installed sibling must come before `PATH`: an agent that installs ACOS
/// into a scratch prefix and runs it from an unrelated directory otherwise
/// silently drives whatever *other* `acos` happens to be on `PATH` — a different
/// build, with different verbs and verdicts. When the resolved CLI answers a
/// different `--version` than this server, that is said once on stderr.
pub fn discover_binary() -> Option<PathBuf> {
    let found = resolve_binary();
    if let Some(path) = found.as_ref() {
        warn_on_version_mismatch(path);
    }
    found
}

fn resolve_binary() -> Option<PathBuf> {
    resolve_binary_from(
        std::env::var("ACOS_BIN").ok(),
        std::env::var_os("ACOS_HOME"),
        std::env::current_exe().ok(),
        std::env::var_os("PATH"),
    )
    .or_else(find_repo_binary)
}

/// The discovery order as a pure function of its inputs (testable without
/// touching the process environment): `ACOS_BIN` > `$ACOS_HOME/bin/acos` >
/// sibling of the executable > `PATH`. The repo walk is the caller's fallback.
fn resolve_binary_from(
    acos_bin: Option<String>,
    acos_home: Option<std::ffi::OsString>,
    exe: Option<PathBuf>,
    path: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(value) = acos_bin {
        if !value.trim().is_empty() {
            return Some(PathBuf::from(value));
        }
    }
    if let Some(home) = acos_home.filter(|v| !v.is_empty()) {
        let candidate = PathBuf::from(home).join("bin").join("acos");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Some(exe) = exe {
        if let Some(sibling) = installed_sibling(&exe, "acos") {
            return Some(sibling);
        }
    }
    if let Some(path) = path {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("acos");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The version word out of `acos --version` output (`acos 0.2.1`), when it
/// differs from `ours`. `None` means "matches" or "unreadable".
fn version_mismatch(version_output: &str, ours: &str) -> Option<String> {
    let theirs = version_output
        .split_whitespace()
        .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    (theirs != ours).then(|| theirs.to_string())
}

/// FB2c: say once, on stderr, that the CLI this server drives is a different
/// version than the server itself.
///
/// This is the failure mode that makes an MCP smoke test lie: a scratch-prefix
/// server silently drove another installation's `acos`, which lacked the verb
/// under test. A path is only *proven* by asking it; a CLI that will not answer
/// `--version` is reported as unverified, never as matching.
fn warn_on_version_mismatch(path: &Path) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let ours = env!("CARGO_PKG_VERSION");
        let output = std::process::Command::new(path)
            .arg("--version")
            .output();
        match output {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                if let Some(theirs) = version_mismatch(&text, ours) {
                    eprintln!(
                        "acos-mcp: warning: backend CLI {} reports version {theirs} but acos-mcp is \
                         {ours}; a reply may describe a different build than this server",
                        path.display()
                    );
                }
            }
            _ => eprintln!(
                "acos-mcp: warning: backend CLI {} did not answer `--version`; its build cannot be \
                 verified against acos-mcp {ours}",
                path.display()
            ),
        }
    });
}

/// `<exe dir>/<name>` when that file exists and is not the executable itself.
pub(crate) fn installed_sibling(exe: &Path, name: &str) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let candidate = dir.join(name);
    if candidate.is_file() && candidate != exe {
        Some(candidate)
    } else {
        None
    }
}

fn find_repo_binary() -> Option<PathBuf> {
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        bases.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            bases.push(parent.to_path_buf());
        }
    }
    for base in bases {
        let mut dir: Option<&Path> = Some(base.as_path());
        while let Some(current) = dir {
            for profile in ["release", "debug"] {
                let candidate = current.join("target").join(profile).join("acos");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            dir = current.parent();
        }
    }
    None
}

/// The CLI-backed backend.
pub struct CliBackend {
    config: BackendConfig,
    caps: RwLock<Capabilities>,
}

impl CliBackend {
    /// Create a backend and run the initial capability probe.
    pub fn new(config: BackendConfig) -> Self {
        let caps = probe(&config);
        CliBackend {
            config,
            caps: RwLock::new(caps),
        }
    }

    /// The backend configuration.
    pub fn config(&self) -> &BackendConfig {
        &self.config
    }

    fn resolved_binary(&self) -> Option<PathBuf> {
        self.config
            .binary
            .clone()
            .or_else(discover_binary)
    }

    fn call_inspect(
        &self,
        binary: &Path,
        request_id: &str,
        args: &Value,
        ctx: &CallContext,
    ) -> Envelope {
        let Some(apk) = args.get("apk").and_then(Value::as_str) else {
            return invalid_argument(request_id, "apk", "missing required string `apk`");
        };
        let apk_path = match safety::validate_path(apk, &ctx.allowed_roots) {
            Ok(path) => path,
            Err(reason) => return unsafe_path(request_id, apk, &reason),
        };
        let argv = vec![
            "inspect".to_string(),
            apk_path.display().to_string(),
            "--json".to_string(),
        ];
        let captured = match run_child(binary, &argv, &self.config.extra_env, ctx) {
            Ok(captured) => captured,
            Err(err) => return run_error_envelope(request_id, CommandName::Inspect, err, ctx),
        };
        envelope_from_captured(&captured, request_id, CommandName::Inspect, ctx)
    }

    fn call_run_dex(
        &self,
        binary: &Path,
        request_id: &str,
        args: &Value,
        ctx: &CallContext,
    ) -> Envelope {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return invalid_argument(request_id, "path", "missing required string `path`");
        };
        let dex_path = match safety::validate_path(path, &ctx.allowed_roots) {
            Ok(path) => path,
            Err(reason) => return unsafe_path(request_id, path, &reason),
        };
        let main = args
            .get("main")
            .and_then(Value::as_str)
            .unwrap_or("Main")
            .to_string();
        let mut argv = vec![
            "run-dex".to_string(),
            dex_path.display().to_string(),
            "--main".to_string(),
            main,
            "--json".to_string(),
        ];
        if let Some(program_args) = args.get("args").and_then(Value::as_array) {
            for arg in program_args {
                if let Some(value) = arg.as_str() {
                    argv.push("--arg".to_string());
                    argv.push(value.to_string());
                }
            }
        }

        let mut env = self.config.extra_env.clone();
        let heap = args
            .get("heap_max_bytes")
            .and_then(Value::as_u64)
            .or_else(|| ctx.budget.memory_mib.map(|mib| mib.saturating_mul(1024 * 1024)));
        if let Some(heap) = heap {
            env.push(("ACOS_HEAP_MAX".to_string(), heap.to_string()));
        }
        if args
            .get("gc_stress")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            env.push(("ACOS_GC_STRESS".to_string(), "1".to_string()));
        }

        let captured = match run_child(binary, &argv, &env, ctx) {
            Ok(captured) => captured,
            Err(err) => return run_error_envelope(request_id, CommandName::RunDex, err, ctx),
        };
        envelope_from_captured(&captured, request_id, CommandName::RunDex, ctx)
    }

    fn call_verify(
        &self,
        binary: &Path,
        request_id: &str,
        args: &Value,
        ctx: &CallContext,
    ) -> Envelope {
        let Some(scenario) = args.get("scenario").and_then(Value::as_str) else {
            return invalid_argument(request_id, "scenario", "missing required string `scenario`");
        };
        let scenario_path = match safety::validate_path(scenario, &ctx.allowed_roots) {
            Ok(path) => path,
            Err(reason) => return unsafe_path(request_id, scenario, &reason),
        };

        let mut argv = vec![
            "verify".to_string(),
            scenario_path.display().to_string(),
            "--json".to_string(),
        ];
        if let Some(apk) = args.get("apk").and_then(Value::as_str) {
            match safety::validate_path(apk, &ctx.allowed_roots) {
                Ok(path) => {
                    argv.push("--apk".to_string());
                    argv.push(path.display().to_string());
                }
                Err(reason) => return unsafe_path(request_id, apk, &reason),
            }
        }
        if let Some(artifacts) = args.get("artifacts").and_then(Value::as_str) {
            match safety::validate_path(artifacts, &ctx.allowed_roots) {
                Ok(path) => {
                    argv.push("--artifacts".to_string());
                    argv.push(path.display().to_string());
                }
                Err(reason) => return unsafe_path(request_id, artifacts, &reason),
            }
        }
        // FB4: a run owns its artifacts directory. The CLI refuses a non-empty
        // directory another run wrote unless `--overwrite` is passed; relay the
        // caller's explicit choice (default: refuse) unchanged.
        if args
            .get("overwrite")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            argv.push("--overwrite".to_string());
        }
        if let Some(reference) = args.get("reference").and_then(Value::as_str) {
            match safety::validate_path(reference, &ctx.allowed_roots) {
                Ok(path) => {
                    argv.push("--reference".to_string());
                    argv.push(path.display().to_string());
                }
                Err(reason) => return unsafe_path(request_id, reference, &reason),
            }
        }
        if let Some(fidelity) = args.get("required_fidelity").and_then(Value::as_str) {
            argv.push("--required-fidelity".to_string());
            argv.push(fidelity.to_string());
        }
        // Engine selection: **real ART by default**, exactly like every other
        // MCP session tool. `acos verify`'s own CLI default is the frozen Rust
        // VM, which cannot honour an ART-only scenario (for example
        // `animations: off`), so the tool must pass the engine explicitly.
        // `engine: "rust-vm"` is the only way to get the frozen VM and every
        // reply then says so. A `reference` replay is a recorded-evidence run,
        // not live ART, and cannot be combined with `--engine art`, so it keeps
        // the CLI default.
        let engine = match args.get("engine").and_then(Value::as_str) {
            Some(value) => Some(value.to_string()),
            // The **one** default engine (`acos_protocol::commands`), the same
            // constant the schema advertises and `acos verify --help` prints.
            // Hard-coding "art" here is how help text and behaviour drifted
            // apart in the first place (feedback 2026-11-14).
            None if args.get("reference").is_none() => Some(
                acos_protocol::commands::DEFAULT_VERIFY_ENGINE_NAME.to_string(),
            ),
            None => None,
        };
        if let Some(engine) = engine {
            argv.push("--engine".to_string());
            argv.push(engine);
        }
        // F10: `flags` are deterministic `NAME=VALUE` metadata recorded in the
        // reproducer (`repro.json` + `repro.sh`), not CLI options. A malformed
        // entry (`--native`, `engine=`) is a structured INVALID_ARGUMENT here,
        // instead of being passed to the CLI as `--flag --native` (which clap
        // rejects as "a value is required").
        if let Some(flags) = args.get("flags") {
            let Some(flags) = flags.as_array() else {
                return invalid_argument(request_id, "flags", "`flags` must be an array of NAME=VALUE strings");
            };
            for (index, flag) in flags.iter().enumerate() {
                let Some(flag) = flag.as_str() else {
                    return invalid_argument(
                        request_id,
                        "flags",
                        &format!("`flags[{index}]` is not a string"),
                    );
                };
                if let Err(reason) = acos_protocol::commands::validate_reproducer_flag(flag) {
                    return Envelope::failure(
                        request_id,
                        ProtocolBackend::AcosNative,
                        Fidelity::NativeRuntime,
                        ProtocolError::new(
                            ErrorCode::InvalidArgument,
                            format!("invalid argument `flags[{index}]`: {reason}"),
                        )
                        .with_hint(
                            "Flags are recorded in the reproducer as NAME=VALUE (`engine=art`). \
                             They are not acos options: pass `engine` for the engine.",
                        )
                        .with_details(
                            json!({"field": "flags", "index": index, "value": flag, "reason": reason}),
                        ),
                        Metrics::wall_ms(0),
                    );
                }
                argv.push("--flag".to_string());
                argv.push(flag.to_string());
            }
        }

        let captured = match run_child(binary, &argv, &self.config.extra_env, ctx) {
            Ok(captured) => captured,
            Err(err) => return run_error_envelope(request_id, CommandName::Verify, err, ctx),
        };
        envelope_from_captured(&captured, request_id, CommandName::Verify, ctx)
    }

    /// Run `acos doctor --json` for the `acos_doctor` server tool.
    ///
    /// The report is the raw `acos doctor --json` object, not an ACOS envelope.
    /// A host that fails the preflight returns `fail` with `ASSERTION_FAILED`
    /// (an `ASSERTION_FAILED` code) and the report under `result`, so an agent
    /// can read each failing check and its `fix` string.
    pub fn call_doctor(&self, request_id: &str, args: &Value, ctx: &CallContext) -> Envelope {
        let Some(binary) = self.resolved_binary() else {
            return Envelope::failure(
                request_id,
                command_backend(CommandName::Inspect),
                Fidelity::StaticAnalysis,
                ProtocolError::new(
                    ErrorCode::UnsupportedBackend,
                    "no `acos` binary found, so `acos doctor` cannot run",
                )
                .with_hint("Set ACOS_BIN to the built `acos` binary.")
                .with_details(json!({})),
                Metrics::wall_ms(0),
            );
        };
        let mut argv = vec!["doctor".to_string(), "--json".to_string()];
        if let Some(sdk) = args.get("sdk").and_then(Value::as_str) {
            match safety::validate_path(sdk, &ctx.allowed_roots) {
                Ok(path) => {
                    argv.push("--sdk".to_string());
                    argv.push(path.display().to_string());
                }
                Err(reason) => return unsafe_path(request_id, sdk, &reason),
            }
        }
        if let Some(build_tools) = args.get("build_tools").and_then(Value::as_str) {
            argv.push("--build-tools".to_string());
            argv.push(build_tools.to_string());
        }
        let captured = match run_child(&binary, &argv, &self.config.extra_env, ctx) {
            Ok(captured) => captured,
            Err(err) => return run_error_envelope(request_id, CommandName::Inspect, err, ctx),
        };
        let stdout = String::from_utf8_lossy(&captured.stdout);
        let parsed = serde_json::from_str::<Value>(stdout.trim());
        match parsed {
            Ok(value) if value.is_object() => {
                let healthy = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
                let metrics = Metrics::wall_ms(captured.wall_ms);
                let failures: Vec<Value> = value
                    .get("checks")
                    .and_then(Value::as_array)
                    .map(|checks| {
                        checks
                            .iter()
                            .filter(|check| {
                                check.get("status").and_then(Value::as_str) == Some("fail")
                            })
                            .filter_map(|check| check.get("name").cloned())
                            .collect()
                    })
                    .unwrap_or_default();
                let envelope = if healthy {
                    Envelope::pass(
                        request_id,
                        command_backend(CommandName::Inspect),
                        Fidelity::StaticAnalysis,
                        metrics,
                    )
                } else {
                    Envelope::failure(
                        request_id,
                        command_backend(CommandName::Inspect),
                        Fidelity::StaticAnalysis,
                        ProtocolError::new(
                            ErrorCode::AssertionFailed,
                            "`acos doctor` found failing host preflight checks",
                        )
                        .with_hint(
                            "Read `result.checks` for the failing `name`s and their `fix` strings.",
                        )
                        .with_details(json!({ "failures": failures })),
                        metrics,
                    )
                };
                envelope.with_value_result(value)
            }
            _ => {
                let stderr = String::from_utf8_lossy(&captured.stderr);
                let detail = stderr.trim();
                let message = if detail.is_empty() {
                    format!(
                        "`acos doctor` produced no JSON report (exit {:?})",
                        captured.status
                    )
                } else {
                    format!(
                        "`acos doctor` failed (exit {:?}): {}",
                        captured.status,
                        truncate(detail, 2000)
                    )
                };
                Envelope::failure(
                    request_id,
                    command_backend(CommandName::Inspect),
                    Fidelity::StaticAnalysis,
                    ProtocolError::new(ErrorCode::Internal, message),
                    Metrics::wall_ms(captured.wall_ms),
                )
            }
        }
    }
}

impl Backend for CliBackend {
    fn call(&self, command: CommandName, args: &Value, ctx: &CallContext) -> Envelope {
        let request_id = args
            .pointer("/base/request_id")
            .and_then(Value::as_str)
            .unwrap_or("req_mcp")
            .to_string();

        if let Err(reason) = safety::check_arg_limits(args, ctx.max_arg_chars) {
            return invalid_argument(&request_id, "$", &reason);
        }
        // FB5: a relative path names a file in the *client's* cwd, which is not
        // this server's cwd. Refuse it and say what the server would have used.
        if let Some(envelope) =
            safety::relative_path_envelope(&request_id, args, Fidelity::StaticAnalysis)
        {
            return envelope;
        }

        let caps = self.capabilities();
        if !caps.is_supported(command) {
            return unsupported_envelope(&request_id, command, &caps);
        }
        let Some(binary) = self.resolved_binary() else {
            return unsupported_envelope(&request_id, command, &caps);
        };

        match command {
            CommandName::Inspect => self.call_inspect(&binary, &request_id, args, ctx),
            CommandName::RunDex => self.call_run_dex(&binary, &request_id, args, ctx),
            CommandName::Verify => self.call_verify(&binary, &request_id, args, ctx),
            _ => unsupported_envelope(&request_id, command, &caps),
        }
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn refresh_capabilities(&self) -> Capabilities {
        let caps = probe(&self.config);
        *self.caps.write().unwrap_or_else(|e| e.into_inner()) = caps.clone();
        caps
    }

    fn info(&self) -> BackendInfo {
        let caps = self.capabilities();
        BackendInfo {
            binary: caps.binary,
            version: caps.version,
        }
    }

    /// The bare CLI backend still serves `acos_doctor`: it delegates to
    /// `acos doctor --json` and holds no session state.
    fn session_call(&self, name: &str, args: &Value, ctx: &CallContext) -> Option<Envelope> {
        if name != "acos_doctor" {
            return None;
        }
        let request_id = args
            .pointer("/base/request_id")
            .and_then(Value::as_str)
            .unwrap_or("req_mcp")
            .to_string();
        Some(self.call_doctor(&request_id, args, ctx))
    }
}

fn run_child(
    binary: &Path,
    argv: &[String],
    extra_env: &[(String, String)],
    ctx: &CallContext,
) -> Result<Captured, RunError> {
    let spec = ChildSpec {
        program: binary,
        args: argv,
        env: extra_env,
        deadline: ctx.deadline,
        cancel: &ctx.cancel,
        max_output_bytes: ctx.max_output_bytes,
    };
    process::run(&spec)
}

fn probe(config: &BackendConfig) -> Capabilities {
    let Some(binary) = config.binary.clone().or_else(discover_binary) else {
        return Capabilities {
            binary: None,
            version: None,
            probed: true,
            supported: HashSet::new(),
            probe_error: Some(
                "no `acos` binary found; set ACOS_BIN or build acos-cli".to_string(),
            ),
        };
    };

    let cancel = CancelToken::new();
    let help_args = vec!["--help".to_string()];
    let help_spec = ChildSpec {
        program: &binary,
        args: &help_args,
        env: &config.extra_env,
        deadline: Instant::now() + config.probe_timeout,
        cancel: &cancel,
        max_output_bytes: config.max_output_bytes,
    };
    let help = process::run(&help_spec);

    let version = {
        let version_args = vec!["--version".to_string()];
        let version_spec = ChildSpec {
            program: &binary,
            args: &version_args,
            env: &config.extra_env,
            deadline: Instant::now() + config.probe_timeout,
            cancel: &cancel,
            max_output_bytes: config.max_output_bytes,
        };
        process::run(&version_spec)
            .ok()
            .map(|captured| String::from_utf8_lossy(&captured.stdout).trim().to_string())
            .filter(|v| !v.is_empty())
    };

    match help {
        Ok(captured) => {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&captured.stdout),
                String::from_utf8_lossy(&captured.stderr)
            );
            let supported = parse_help_commands(&text);
            Capabilities {
                binary: Some(binary),
                version,
                probed: true,
                supported,
                probe_error: None,
            }
        }
        Err(err) => Capabilities {
            binary: Some(binary),
            version,
            probed: true,
            supported: HashSet::new(),
            probe_error: Some(format!("capability probe failed: {err}")),
        },
    }
}

/// Parse command names out of a clap `--help` listing.
pub fn parse_help_commands(text: &str) -> HashSet<CommandName> {
    let mut supported = HashSet::new();
    let mut in_commands = false;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.eq_ignore_ascii_case("commands:") {
            in_commands = true;
            continue;
        }
        if in_commands {
            if trimmed.trim().is_empty() {
                in_commands = false;
                continue;
            }
            // clap prints `  <name>  <about>` (two-space indent). Stop at the
            // next section header (e.g. `Options:`).
            if !line.starts_with(' ') && !line.starts_with('\t') {
                in_commands = false;
                continue;
            }
            if let Some(name) = line.split_whitespace().next() {
                if name == "help" {
                    continue;
                }
                if let Some(command) = crate::descriptors::command_from_wire(name) {
                    supported.insert(command);
                }
            }
        }
    }
    supported
}

fn envelope_from_captured(
    captured: &Captured,
    fallback_id: &str,
    command: CommandName,
    ctx: &CallContext,
) -> Envelope {
    let stdout = String::from_utf8_lossy(&captured.stdout);
    let parsed = serde_json::from_str::<Value>(stdout.trim());
    let mut envelope = match parsed {
        Ok(value) if value.is_object() => envelope_from_cli_value(&value, fallback_id, command),
        _ => {
            let stderr = String::from_utf8_lossy(&captured.stderr);
            let detail = stderr.trim();
            let message = if detail.is_empty() {
                format!(
                    "`acos {}` produced no JSON envelope (exit {:?})",
                    command.as_str(),
                    captured.status
                )
            } else {
                format!(
                    "`acos {}` failed (exit {:?}): {}",
                    command.as_str(),
                    captured.status,
                    truncate(detail, 2000)
                )
            };
            Envelope::failure(
                fallback_id,
                command_backend(command),
                command_fidelity(command),
                ProtocolError::new(ErrorCode::Internal, message),
                Metrics::wall_ms(captured.wall_ms),
            )
        }
    };

    // Enforce the declared memory budget when the backend reports peak RSS.
    if let (Some(mib), Some(peak)) = (ctx.budget.memory_mib, envelope.metrics().peak_rss_bytes) {
        let limit = mib.saturating_mul(1024 * 1024);
        if peak > limit {
            let request_id = envelope.request_id().to_string();
            envelope = Envelope::failure(
                request_id,
                command_backend(command),
                command_fidelity(command),
                ProtocolError::new(
                    ErrorCode::ResourceLimit,
                    format!("peak RSS {peak} bytes exceeded the declared budget of {limit} bytes"),
                )
                .with_details(json!({"peak_rss_bytes": peak, "budget_bytes": limit})),
                envelope.metrics(),
            );
        }
    }

    envelope
}

/// Convert a raw `acos --json` object into a validated protocol envelope.
///
/// The CLI's error objects do not carry `hint`/`retryable`, so the envelope is
/// rebuilt through the protocol constructors rather than deserialized.
pub fn envelope_from_cli_value(value: &Value, fallback_id: &str, command: CommandName) -> Envelope {
    let request_id = value
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or(fallback_id);
    let backend = value
        .get("backend")
        .and_then(Value::as_str)
        .and_then(parse_backend)
        .unwrap_or_else(|| command_backend(command));
    let fidelity = value
        .get("fidelity")
        .and_then(Value::as_str)
        .and_then(parse_fidelity)
        .unwrap_or_else(|| command_fidelity(command));
    let metrics = serde_json::from_value::<Metrics>(
        value.get("metrics").cloned().unwrap_or_else(|| json!({})),
    )
    .unwrap_or_else(|_| Metrics::wall_ms(0));

    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let mut envelope = if ok {
        Envelope::pass(request_id, backend, fidelity, metrics)
    } else {
        let code = value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .and_then(ErrorCode::from_str_opt)
            .unwrap_or(ErrorCode::Internal);
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("the acos backend reported an error");
        let mut error = ProtocolError::new(code, message);
        if let Some(details) = value.pointer("/error/details") {
            if !details.is_null() {
                error = error.with_details(details.clone());
            }
        }
        Envelope::failure(request_id, backend, fidelity, error, metrics)
    };

    // Attach the optional parts the same way for pass and failure so the MCP
    // envelope is the CLI envelope (for example a `verify` fail still carries its
    // verdict result).
    if let Some(result) = value.get("result") {
        if !result.is_null() {
            envelope = envelope.with_value_result(result.clone());
        }
    }
    if let Some(caps) = value.get("capabilities_used").and_then(Value::as_array) {
        envelope =
            envelope.with_capabilities(caps.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if let Some(session) = value.get("session_id").and_then(Value::as_str) {
        envelope = envelope.with_session_id(session);
    }
    if let Some(isolation) = value
        .get("isolation")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
    {
        envelope = envelope.with_isolation(isolation);
    }
    if let Some(evidence) = value.get("evidence") {
        if let Ok(evidence) = serde_json::from_value(evidence.clone()) {
            envelope = envelope.with_evidence(evidence);
        }
    }
    envelope
}

fn parse_backend(value: &str) -> Option<ProtocolBackend> {
    serde_json::from_value(Value::String(value.to_string())).ok()
}

fn parse_fidelity(value: &str) -> Option<Fidelity> {
    serde_json::from_value(Value::String(value.to_string())).ok()
}

fn unsupported_envelope(request_id: &str, command: CommandName, caps: &Capabilities) -> Envelope {
    let reason = caps
        .probe_error
        .clone()
        .unwrap_or_else(|| "the acos backend does not implement this command yet".to_string());
    let error = ProtocolError::new(
        ErrorCode::UnsupportedBackend,
        format!(
            "command `{}` is not implemented by the acos backend: {reason}",
            command.as_str()
        ),
    )
    .with_hint(format!(
        "This ACOS build does not implement `{}`. Call `acos_capabilities` for the current supported set. \
         This is `unsupported`, never a pass; do not rewrite the app to hide the gap.",
        command.as_str()
    ))
    .with_details(json!({
        "command": command.as_str(),
        "tool": command.tool_name(),
        "available": false,
        "binary": caps.binary.as_ref().map(|p| p.display().to_string()),
        "probe_error": caps.probe_error,
    }));
    Envelope::failure(
        request_id,
        command_backend(command),
        command_fidelity(command),
        error,
        Metrics::wall_ms(0),
    )
}

fn invalid_argument(request_id: &str, field: &str, reason: &str) -> Envelope {
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::StaticAnalysis,
        ProtocolError::new(
            ErrorCode::InvalidArgument,
            format!("invalid argument `{field}`: {reason}"),
        )
        .with_details(json!({"field": field, "reason": reason})),
        Metrics::wall_ms(0),
    )
}

fn unsafe_path(request_id: &str, path: &str, reason: &str) -> Envelope {
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::StaticAnalysis,
        ProtocolError::new(ErrorCode::UnsafePath, reason.to_string())
            .with_details(json!({"path": path})),
        Metrics::wall_ms(0),
    )
}

fn run_error_envelope(
    request_id: &str,
    command: CommandName,
    err: RunError,
    ctx: &CallContext,
) -> Envelope {
    let (code, message) = match &err {
        RunError::Timeout => (
            ErrorCode::Timeout,
            format!(
                "`acos {}` exceeded its wall-clock budget of {} ms",
                command.as_str(),
                ctx.deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
            ),
        ),
        RunError::Cancelled => (
            ErrorCode::Internal,
            format!("`acos {}` was cancelled by the client", command.as_str()),
        ),
        RunError::OutputLimit => (
            ErrorCode::ResourceLimit,
            format!(
                "`acos {}` produced more than {} bytes on one stream",
                command.as_str(),
                ctx.max_output_bytes
            ),
        ),
        RunError::Spawn(detail) => (
            ErrorCode::Internal,
            format!("could not start `acos {}`: {detail}", command.as_str()),
        ),
        RunError::Io(detail) => (
            ErrorCode::Internal,
            format!("`acos {}` I/O failure: {detail}", command.as_str()),
        ),
    };
    let mut error = ProtocolError::new(code, message);
    if matches!(err, RunError::Cancelled) {
        error = error.with_details(json!({"cancelled": true}));
    }
    Envelope::failure(
        request_id,
        command_backend(command),
        command_fidelity(command),
        error,
        Metrics::wall_ms(0),
    )
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_cli_beside_this_binary_wins_over_path() {
        // The REL1a layout: <prefix>/bin/acos-mcp sits beside <prefix>/bin/acos.
        let dir = std::env::temp_dir().join(format!("acos-sibling-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("acos-mcp");
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(installed_sibling(&exe, "acos"), None, "nothing beside it yet");
        let sibling = dir.join("acos");
        std::fs::write(&sibling, b"").unwrap();
        assert_eq!(installed_sibling(&exe, "acos"), Some(sibling.clone()));
        assert_eq!(
            installed_sibling(&sibling, "acos"),
            None,
            "the executable is never its own backend"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// FB2c item 7: the discovery order is ACOS_BIN > ACOS_HOME > sibling > PATH.
    #[test]
    fn discovery_order_is_bin_then_home_then_sibling_then_path() {
        let dir = std::env::temp_dir().join(format!("acos-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mk = |rel: &str| {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
            path
        };
        let home_acos = mk("home/bin/acos");
        let sibling = mk("prefix/bin/acos");
        let exe = mk("prefix/bin/acos-mcp");
        let on_path = mk("pathdir/acos");
        let path = std::env::join_paths([dir.join("pathdir")]).unwrap();
        let home = Some(dir.join("home").into_os_string());
        let all = |bin: Option<String>, home: Option<std::ffi::OsString>| {
            resolve_binary_from(bin, home, Some(exe.clone()), Some(path.clone()))
        };
        assert_eq!(
            all(Some("/x/acos".into()), home.clone()),
            Some(PathBuf::from("/x/acos")),
            "ACOS_BIN beats everything"
        );
        assert_eq!(all(None, home.clone()), Some(home_acos), "explicit ACOS_HOME beats the sibling");
        assert_eq!(all(None, None), Some(sibling.clone()), "the sibling beats PATH");
        assert_eq!(
            all(Some("  ".into()), Some(dir.join("nohome").into_os_string())),
            Some(sibling),
            "a blank ACOS_BIN and an ACOS_HOME without a CLI fall through"
        );
        let lone = mk("lone/acos-mcp");
        assert_eq!(
            resolve_binary_from(None, None, Some(lone), Some(path)),
            Some(on_path),
            "PATH is last"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// FB2c item 7: a version mismatch is detected from `--version`; a match or
    /// an unreadable answer is not reported as a mismatch.
    #[test]
    fn a_cli_version_mismatch_is_detected() {
        assert_eq!(version_mismatch("acos 0.2.0\n", "0.2.1"), Some("0.2.0".to_string()));
        assert_eq!(version_mismatch("acos 0.2.1\n", "0.2.1"), None);
        assert_eq!(version_mismatch("garbage", "0.2.1"), None);
    }

    #[test]
    fn help_parsing_finds_clap_commands() {
        let text = "The `acos` command-line interface.\n\nUsage: acos <COMMAND>\n\nCommands:\n  inspect  Inspect an APK\n  run-dex  Execute a DEX main\n  help     Print help\n\nOptions:\n  -h, --help  Print help\n";
        let supported = parse_help_commands(text);
        assert!(supported.contains(&CommandName::Inspect));
        assert!(supported.contains(&CommandName::RunDex));
        assert!(!supported.contains(&CommandName::Run));
        assert_eq!(supported.len(), 2);
    }

    #[test]
    fn cli_pass_envelope_is_rebuilt() {
        let value = json!({
            "request_id": "req_1",
            "ok": true,
            "status": "pass",
            "backend": "acos-native",
            "fidelity": "static-analysis",
            "capabilities_used": ["apk.inspect"],
            "error": null,
            "result": {"package": "x"},
            "metrics": {"wall_ms": 3}
        });
        let env = envelope_from_cli_value(&value, "fallback", CommandName::Inspect);
        assert!(env.ok());
        assert_eq!(env.status(), acos_protocol::Status::Pass);
        assert_eq!(env.fidelity(), Fidelity::StaticAnalysis);
        assert_eq!(env.result().unwrap()["package"], "x");
    }

    #[test]
    fn cli_error_envelope_gets_hint_and_status() {
        let value = json!({
            "request_id": "req_2",
            "ok": false,
            "status": "unsupported",
            "backend": "acos-native",
            "fidelity": "native-runtime",
            "capabilities_used": ["dex.run"],
            "error": {"code": "UNSUPPORTED_JAVA_API", "message": "java.lang.reflect.Proxy"},
            "metrics": {"wall_ms": 1}
        });
        let env = envelope_from_cli_value(&value, "fallback", CommandName::RunDex);
        assert!(!env.ok());
        assert_eq!(env.status(), acos_protocol::Status::Unsupported);
        let error = env.error().unwrap();
        assert_eq!(error.code, ErrorCode::UnsupportedJavaApi);
        assert!(!error.hint.is_empty());
    }

    /// FB4: the MCP `acos_verify` tool runs the CLI, so the run-owns-its-dir
    /// rule and its explicit `--overwrite` opt-in must be relayed verbatim. A
    /// fake binary records the argv it receives.
    #[test]
    fn verify_relays_the_explicit_overwrite_choice() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "acos-mcp-verify-argv-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let argv_file = dir.join("argv.txt");
        let script = dir.join("fake-acos.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 case \"$1\" in\n\
                   --help) echo 'Usage: acos <COMMAND>'; echo 'Commands:'; echo '  verify  Run a scenario'; exit 0;;\n\
                   --version) echo 'acos 0.2.0'; exit 0;;\n\
                 esac\n\
                 printf '%s\\n' \"$@\" > {}\n\
                 echo '{{}}'\n",
                argv_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let scenario = dir.join("s.yaml");
        std::fs::write(&scenario, "version: 1\n").unwrap();
        let backend = CliBackend::new(BackendConfig::for_binary(&script));
        let ctx = CallContext {
            cancel: crate::CancelToken::new(),
            deadline: Instant::now() + Duration::from_secs(10),
            budget: ResourceBudget::default(),
            allowed_roots: vec![dir.clone()],
            artifacts_dir: dir.clone(),
            max_arg_chars: 4096,
            max_output_bytes: 4096,
        };
        let args = json!({
            "base": {"request_id": "req_1"},
            "scenario": scenario.display().to_string(),
            "artifacts": dir.join("art").display().to_string(),
        });
        backend.call(CommandName::Verify, &args, &ctx);
        let argv = std::fs::read_to_string(&argv_file).expect("the fake binary ran");
        assert!(argv.contains("--engine\nart"), "ART stays the MCP default: {argv}");
        assert!(
            !argv.split('\n').any(|arg| arg == "--overwrite"),
            "overwrite is off by default: {argv}"
        );

        let mut explicit = args.clone();
        explicit["overwrite"] = json!(true);
        backend.call(CommandName::Verify, &explicit, &ctx);
        let argv = std::fs::read_to_string(&argv_file).unwrap();
        assert!(
            argv.split('\n').any(|arg| arg == "--overwrite"),
            "the caller's explicit overwrite is relayed: {argv}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_backend_envelope_is_never_a_pass() {
        let caps = Capabilities::unprobed();
        let env = unsupported_envelope("req_3", CommandName::Verify, &caps);
        assert!(!env.ok());
        assert_eq!(env.status(), acos_protocol::Status::Unsupported);
        assert_eq!(env.error().unwrap().code, ErrorCode::UnsupportedBackend);
        assert_eq!(env.error().unwrap().details.as_ref().unwrap()["available"], false);
    }
}
