//! Session identity, isolation, display and resource-budget types shared by
//! every command.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How independent a session is from other sessions.
///
/// The levels are ordered from cheapest/weakest to strongest. A claim of data
/// independence is only allowed at `sandbox` and above, and only when the
/// engine actually implements the guarantees (see `docs/AGENT_API.md`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum IsolationLevel {
    /// Another activity/task in a shared process and shared app data. No isolation.
    SharedTask,
    /// Separate process, potentially the same package/storage identity.
    Process,
    /// Separate process/UID/credentials/mount namespace and app data.
    Sandbox,
    /// Fully separate engine instance (strongest short of a VM).
    Core,
    /// Sandboxed full Android/backend for untrusted or device-level fidelity.
    Vm,
}

impl IsolationLevel {
    /// Numeric rank, useful for "at least this strong" checks.
    pub const fn rank(self) -> u8 {
        match self {
            IsolationLevel::SharedTask => 0,
            IsolationLevel::Process => 1,
            IsolationLevel::Sandbox => 2,
            IsolationLevel::Core => 3,
            IsolationLevel::Vm => 4,
        }
    }

    /// True only where the engine can claim independent app data.
    pub const fn guarantees_independent_data(self) -> bool {
        self.rank() >= IsolationLevel::Sandbox.rank()
    }

    /// The stable wire string.
    pub const fn as_str(self) -> &'static str {
        match self {
            IsolationLevel::SharedTask => "shared-task",
            IsolationLevel::Process => "process",
            IsolationLevel::Sandbox => "sandbox",
            IsolationLevel::Core => "core",
            IsolationLevel::Vm => "vm",
        }
    }
}

impl std::fmt::Display for IsolationLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The display a session renders to.
///
/// `none` is headless: capture/observe must return `NO_DISPLAY` instead of
/// silently allocating a surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum DisplaySpec {
    /// No display at all (headless logic/storage tests).
    None,
    /// Offscreen software surface with an explicit size and density.
    Offscreen {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// Density in dots per inch (for example 420).
        density_dpi: u32,
    },
}

impl DisplaySpec {
    /// Convenience constructor.
    pub const fn offscreen(width: u32, height: u32, density_dpi: u32) -> Self {
        DisplaySpec::Offscreen {
            width,
            height,
            density_dpi,
        }
    }

    /// True when no display exists.
    pub const fn is_headless(self) -> bool {
        matches!(self, DisplaySpec::None)
    }

    /// `WxH@dpi`, or `none`.
    pub fn label(self) -> String {
        match self {
            DisplaySpec::None => "none".to_string(),
            DisplaySpec::Offscreen {
                width,
                height,
                density_dpi,
            } => format!("{width}x{height}@{density_dpi}"),
        }
    }
}

/// Hard resource budgets for one request or one scenario.
///
/// Every field is opt-in; `None` means "use the engine default". The engine
/// must return `RESOURCE_LIMIT`/`TIMEOUT` when a budget is exhausted instead of
/// silently exceeding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ResourceBudget {
    /// Maximum resident memory attributable to this request/session, in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u64>,
    /// Maximum wall-clock time, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
    /// Maximum CPU time, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<u64>,
}

impl ResourceBudget {
    /// A budget with only a wall-clock limit.
    pub const fn wall_ms(ms: u64) -> Self {
        ResourceBudget {
            memory_mib: None,
            wall_ms: Some(ms),
            cpu_ms: None,
        }
    }

    /// A budget with only a memory limit.
    pub const fn memory_mib(mib: u64) -> Self {
        ResourceBudget {
            memory_mib: Some(mib),
            wall_ms: None,
            cpu_ms: None,
        }
    }

    /// Builder: set the wall limit.
    pub const fn with_wall_ms(mut self, ms: u64) -> Self {
        self.wall_ms = Some(ms);
        self
    }

    /// Builder: set the memory limit.
    pub const fn with_memory_mib(mut self, mib: u64) -> Self {
        self.memory_mib = Some(mib);
        self
    }

    /// Builder: set the CPU limit.
    pub const fn with_cpu_ms(mut self, ms: u64) -> Self {
        self.cpu_ms = Some(ms);
        self
    }

    /// True when no limit is declared.
    pub const fn is_empty(self) -> bool {
        self.memory_mib.is_none() && self.wall_ms.is_none() && self.cpu_ms.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_wire_shape() {
        let none = serde_json::to_value(DisplaySpec::None).unwrap();
        assert_eq!(none, serde_json::json!({"mode": "none"}));
        let off = serde_json::to_value(DisplaySpec::offscreen(1080, 2400, 420)).unwrap();
        assert_eq!(
            off,
            serde_json::json!({"mode": "offscreen", "width": 1080, "height": 2400, "density_dpi": 420})
        );
        assert_eq!(
            DisplaySpec::offscreen(1080, 2400, 420).label(),
            "1080x2400@420"
        );
    }

    #[test]
    fn isolation_ranking() {
        assert!(!IsolationLevel::Process.guarantees_independent_data());
        assert!(IsolationLevel::Sandbox.guarantees_independent_data());
        assert!(IsolationLevel::Vm.rank() > IsolationLevel::Core.rank());
    }
}
