//! The [`confine`] orchestrator and its report types.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::error::ConfineError;
use crate::{linux, probe, Layer};

/// Process-global flag set only when [`Layer::BASE`] is fully active.
static CONFINEMENT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether this process has successfully entered confinement.
///
/// This is the process-reported bit the native loader consults before mapping
/// an untrusted APK `.so`. It is set atomically at the very end of
/// [`confine`], and only when no-new-privs, Landlock and seccomp are all
/// active. A fork inherits it; a fresh `exec` (which seccomp forbids anyway)
/// would not.
pub fn confinement_active() -> bool {
    CONFINEMENT_ACTIVE.load(Ordering::SeqCst)
}

/// A confinement profile: the filesystem surface plus the hygiene knobs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfineProfile {
    /// Paths the confined process may read (and execute) but never write.
    ///
    /// Intended for `/system`-like VFS roots and the extracted APK directory.
    pub read_only_paths: Vec<PathBuf>,
    /// Paths the confined process may fully modify. Intended for the app data
    /// directory only.
    pub read_write_paths: Vec<PathBuf>,
    /// Layers that must be active or [`confine`] fails closed.
    ///
    /// Defaults to [`Layer::NoNewPrivs`], [`Layer::CloseFds`],
    /// [`Layer::SanitizeEnv`], [`Layer::Rlimits`], [`Layer::Landlock`],
    /// [`Layer::Seccomp`].
    pub required: Vec<Layer>,
    /// File descriptors kept open across the fd-hygiene step (default
    /// `[0, 1, 2]`).
    pub keep_fds: Vec<i32>,
    /// The sanitised environment to install after `clearenv`.
    pub env: Vec<(String, String)>,
    /// Resource limits to install.
    pub rlimits: RlimitProfile,
    /// Optional namespaces.
    pub namespaces: NamespacePlan,
}

/// Resource limits installed by [`confine`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RlimitProfile {
    /// `RLIMIT_AS` in bytes (`None` leaves it untouched).
    pub address_space_bytes: Option<u64>,
    /// `RLIMIT_NOFILE` (`None` leaves it untouched).
    pub nofile: Option<u64>,
    /// `RLIMIT_NPROC` (`None` leaves it untouched).
    pub nproc: Option<u64>,
    /// `RLIMIT_CPU` in seconds (`None` leaves it untouched).
    pub cpu_seconds: Option<u64>,
    /// `RLIMIT_CORE` (default `0`: never dump core).
    pub core: u64,
}

impl Default for RlimitProfile {
    fn default() -> Self {
        RlimitProfile {
            address_space_bytes: Some(4 * 1024 * 1024 * 1024),
            nofile: Some(1024),
            nproc: Some(8192),
            cpu_seconds: Some(60),
            core: 0,
        }
    }
}

/// Which optional namespaces to attempt.
///
/// All three are unprivileged only when an unprivileged user namespace can be
/// created first, so `net`/`mount` imply `user`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespacePlan {
    /// Create an unprivileged user namespace and map the caller's uid/gid.
    pub user: bool,
    /// Create a network namespace (loopback only).
    pub net: bool,
    /// Create a mount namespace with private propagation.
    pub mount: bool,
}

impl NamespacePlan {
    /// Whether any namespace was requested.
    pub fn any(&self) -> bool {
        self.user || self.net || self.mount
    }

    /// Request all three (the "best effort" profile).
    pub fn unprivileged_all() -> Self {
        NamespacePlan {
            user: true,
            net: true,
            mount: true,
        }
    }
}

impl Default for ConfineProfile {
    fn default() -> Self {
        ConfineProfile {
            read_only_paths: Vec::new(),
            read_write_paths: Vec::new(),
            required: vec![
                Layer::NoNewPrivs,
                Layer::CloseFds,
                Layer::SanitizeEnv,
                Layer::Rlimits,
                Layer::Landlock,
                Layer::Seccomp,
            ],
            keep_fds: vec![0, 1, 2],
            env: default_env(),
            rlimits: RlimitProfile::default(),
            namespaces: NamespacePlan::default(),
        }
    }
}

impl ConfineProfile {
    /// Builder: add a read-only root.
    pub fn with_read_only(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_only_paths.push(path.into());
        self
    }

    /// Builder: add a read-write root.
    pub fn with_read_write(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_write_paths.push(path.into());
        self
    }

    /// Builder: request all namespaces.
    pub fn with_namespaces(mut self) -> Self {
        self.namespaces = NamespacePlan::unprivileged_all();
        self
    }

    /// A probe profile: nothing is required, every layer is merely attempted
    /// and reported. `read_write` is where any probe scratch dir should live.
    pub fn probe(read_write: impl Into<PathBuf>) -> Self {
        ConfineProfile {
            read_write_paths: vec![read_write.into()],
            required: Vec::new(),
            ..ConfineProfile::default()
        }
    }
}

fn default_env() -> Vec<(String, String)> {
    vec![
        ("PATH".to_string(), "/system/bin:/system/xbin".to_string()),
        ("ANDROID_ROOT".to_string(), "/system".to_string()),
        ("ANDROID_DATA".to_string(), "/data".to_string()),
        ("ANDROID_ASSETS".to_string(), "/system/app".to_string()),
        ("TMPDIR".to_string(), "/data/local/tmp".to_string()),
        ("HOME".to_string(), "/data/local/tmp".to_string()),
        ("LANG".to_string(), "C".to_string()),
        ("LC_ALL".to_string(), "C".to_string()),
    ]
}

/// The status of one layer after a [`confine`] attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerStatus {
    /// The kernel accepted the layer.
    Active,
    /// The layer was attempted but the kernel refused it.
    Inactive,
    /// The layer was required and is not active.
    Failed,
    /// The layer was not requested.
    Skipped,
}

/// One layer's outcome.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayerReport {
    /// Which layer.
    pub layer: Layer,
    /// What happened.
    pub status: LayerStatus,
    /// Optional explanation (kernel error, ABI, rule counts, ...).
    pub detail: Option<String>,
}

/// The full outcome of a [`confine`] attempt.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfinementReport {
    /// Whether the process now counts as confined ([`Layer::BASE`] all active).
    pub active: bool,
    /// Landlock ABI observed during the attempt.
    pub landlock_abi: Option<u32>,
    /// Kernel release observed during the attempt.
    pub kernel_release: String,
    /// One entry per layer, in application order.
    pub layers: Vec<LayerReport>,
}

impl ConfinementReport {
    fn new(landlock_abi: Option<u32>, kernel_release: String) -> Self {
        ConfinementReport {
            active: false,
            landlock_abi,
            kernel_release,
            layers: Vec::new(),
        }
    }

    fn set(&mut self, layer: Layer, status: LayerStatus, detail: Option<String>) {
        self.layers.push(LayerReport {
            layer,
            status,
            detail,
        });
    }

    /// Whether `layer` is active in this report.
    pub fn is_active(&self, layer: Layer) -> bool {
        self.layers
            .iter()
            .any(|l| l.layer == layer && l.status == LayerStatus::Active)
    }

    /// The status of `layer`, if present.
    pub fn status(&self, layer: Layer) -> Option<LayerStatus> {
        self.layers.iter().find(|l| l.layer == layer).map(|l| l.status)
    }
}

/// Apply the confinement layers in `profile`.
///
/// Returns a full [`ConfinementReport`] when every required layer is active.
/// If a required layer is missing, returns
/// [`ConfineError::Unavailable`] with code [`CONFINEMENT_UNAVAILABLE`](crate::CONFINEMENT_UNAVAILABLE)
/// — the caller must treat the process as untrusted.
pub fn confine(profile: &ConfineProfile) -> Result<ConfinementReport, ConfineError> {
    let caps = probe::probe();
    let mut report = ConfinementReport::new(caps.landlock_abi, caps.kernel_release);

    // 1. PR_SET_NO_NEW_PRIVS — must precede Landlock and seccomp.
    record(&mut report, Layer::NoNewPrivs, linux::no_new_privs());

    // 2. Close inherited file descriptors.
    record(&mut report, Layer::CloseFds, linux::close_inherited(&profile.keep_fds));

    // 3. Sanitised environment.
    record(&mut report, Layer::SanitizeEnv, linux::sanitize_env(&profile.env));

    // 4. Resource limits.
    record(&mut report, Layer::Rlimits, linux::apply_rlimits(&profile.rlimits));

    // 5. Optional namespaces. Applied before Landlock/seccomp because mapping
    //    uid_map and making mounts private need `/proc` and `mount`, both of
    //    which the later layers would deny.
    let ns = linux::setup_namespaces(&profile.namespaces);
    if ns.is_empty() {
        report.set(Layer::UserNamespace, LayerStatus::Skipped, None);
        report.set(Layer::NetNamespace, LayerStatus::Skipped, None);
        report.set(Layer::MountNamespace, LayerStatus::Skipped, None);
    } else {
        for (layer, result) in ns {
            match result {
                Ok(detail) => report.set(layer, LayerStatus::Active, Some(detail)),
                Err(detail) => report.set(layer, LayerStatus::Failed, Some(detail)),
            }
        }
    }

    // 6. Landlock filesystem ruleset.
    record(
        &mut report,
        Layer::Landlock,
        linux::apply_landlock(&profile.read_only_paths, &profile.read_write_paths),
    );

    // 7. seccomp-bpf syscall filter.
    record(&mut report, Layer::Seccomp, linux::install_seccomp());

    // Fail closed.
    let missing: Vec<Layer> = profile
        .required
        .iter()
        .copied()
        .filter(|l| !report.is_active(*l))
        .collect();
    if !missing.is_empty() {
        let message = report
            .layers
            .iter()
            .filter(|l| missing.contains(&l.layer))
            .map(|l| format!("{}: {}", l.layer, l.detail.as_deref().unwrap_or("unavailable")))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(ConfineError::Unavailable { missing, message });
    }

    if Layer::BASE.iter().all(|l| report.is_active(*l)) {
        CONFINEMENT_ACTIVE.store(true, Ordering::SeqCst);
        report.active = true;
    }
    Ok(report)
}

fn record(report: &mut ConfinementReport, layer: Layer, result: Result<String, String>) {
    match result {
        Ok(detail) => report.set(layer, LayerStatus::Active, Some(detail)),
        Err(detail) => report.set(layer, LayerStatus::Failed, Some(detail)),
    }
}
