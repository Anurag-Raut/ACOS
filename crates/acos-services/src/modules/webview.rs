//! `webview` — a real WebView for the session (ADR-0029 module, ADR-0031
//! broker shape).
//!
//! The framework's own `WebViewFactory.getProvider()` is the entry point; this
//! module owns the two pieces that make it succeed:
//!
//! 1. **the provider system package** — `com.android.webview`, an ordinary
//!    installed package (ADR-0031) whose `com.android.webview.chromium.*`
//!    classes talk to the host over one Unix socket. The launcher installs it
//!    beside the app under test from [`provider_apk`], exactly as it stages any
//!    other system APK. Nothing is registered host-side for it: the package
//!    table *is* the framework's answer (`PackageManager.FEATURE_WEBVIEW`,
//!    `webviewupdate.waitForAndGetProvider`).
//! 2. **the host broker** — one Chromium for the whole host, reached through the
//!    session's gate socket (`<run>/wv/webview.sock`). The gate binds and sleeps;
//!    the broker (and Chromium, ~130–140 MiB once per host) exists only after the
//!    first guest `connect()`, so a session that never creates a `WebView` pays
//!    one socket node and one thread.
//!
//! Both are established at **launch**: the provider APK must be installed before
//! the guest boots (`Application.onCreate` may create a `WebView`), and the
//! socket path is baked into the guest's sandbox. So `attach: at-launch`, and a
//! live `acos_services {op: "attach"}` is refused with the launch call — never a
//! half-attached WebView.
//!
//! # Profiles
//!
//! * `off` (the default: not attached) is Android's own "no WebView provider"
//!   path, unchanged since before this module existed: no provider package in
//!   the image, `hasSystemFeature("android.software.webview")` false,
//!   `WebViewFactory` throwing `UnsupportedOperationException`, no socket. Every
//!   reply that needs a WebView says the module is off.
//! * `real` is the provider plus the lazily started host broker. There is **no
//!   `stub`/`scripted` profile**: a scripted page would be a fake page, which is
//!   the fake success ADR-0029 item 3 forbids. Determinism comes from the real
//!   arm instead — the broker serves the APK's own assets through
//!   `shouldInterceptRequest`, which needs no network.
//!
//! A `real` module without the engine (the `acos-webview` daemon or the pinned
//! `chrome-headless-shell`) is not a pass and not a crash: the gate answers the
//! guest's first frame with the structured `ENGINE_UNAVAILABLE`
//! (`results/handoff/WV6/webview-probe-no-engine.yaml`, preserved as
//! `results/handoff/WV8/webview-probe-no-engine.yaml`).
//!
//! # Frames
//!
//! Frames stay at the engine's **CSS** resolution (device pixels / density) and
//! the wire reports the frame's real pixel size; the guest scales it into the
//! `View`. That is a decision, measured in WV7
//! (`results/handoff/WV7/evidence/engine-capture-facts.txt`): this pinned
//! `chrome-headless-shell` cannot capture at `deviceScaleFactor` under
//! `--enable-begin-frame-control`.
//!
//! # Budget
//!
//! Declared `ram_mib: 4`, `startup_ms: 11`, both measured in WV6
//! (`results/handoff/WV6/REPORT.md` §3): the per-session cost is the gate plus
//! the guest provider context (~4 MiB), the **fixed** cost is one Chromium per
//! host (142.5 MiB PSS / 10 processes, shared by every session), and the first
//! frame is 11.2 ms after the load. The engine's fixed cost is not per session,
//! so it is not in the per-session budget; the manifest says so here and the
//! verdict's `modules[webview].details` repeats it.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::host::ServiceHost;
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass,
};

/// The module id (`acos_launch {"services": [{"id": "webview"}]}`).
pub const MODULE_ID: &str = "webview";

/// The provider package the image installs with this module
/// (`crate::webview::PROVIDER_PACKAGE`, re-exported here so the module and the
/// `webviewupdate` peer cannot name two packages).
pub const PROVIDER_PACKAGE: &str = crate::webview::PROVIDER_PACKAGE;

/// `ModuleConfig` key for the provider APK the launcher stages.
pub const CONFIG_PROVIDER_APK: &str = "provider_apk";

/// Environment override for the provider APK (a test or an operator with a
/// provider built elsewhere).
pub const PROVIDER_APK_ENV: &str = "ACOS_WEBVIEW_PROVIDER_APK";

/// The provider APK as the **image** ships it (`tools/extract-bootclasspath.sh`,
/// which builds it with `tools/build-webview-provider.sh` and stages it under
/// the boot image's `system-apps/`). This is the copy a fresh install has, so it
/// is what a session installs unless an explicit path says otherwise.
pub const PROVIDER_APK_IMAGE_REL: &str = ".cache/boot/android-34/system-apps/com.android.webview.apk";

/// The workspace-relative provider APK `tools/build-apps.sh` produces (the
/// developer/fixture build; the image carries its own copy).
pub const PROVIDER_APK_REL: &str = "fixtures/build/apps/webview-provider.apk";

/// The control ops: the module's own state and what the session's engine is
/// doing. One implementation, two spellings (`acos_service_ctl` over MCP and
/// the CLI reach the same [`Module::control`] arm).
const CONTROL_OPS: [ControlOp; 1] = [ControlOp {
    name: "status",
    args: "none — the provider package and APK this session installs, the gate socket, \
           whether the broker is listening, the frame resolution rule and the measured budget",
}];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "a real WebView: the `com.android.webview` provider package installed in the \
                  session plus the shared host Chromium behind the session's gate socket (one \
                  broker per host, ~130-140 MiB once, started on the first connect); `off` is \
                  Android's own no-provider path, and there is no stub/scripted profile",
        // No `ServiceManager` name is this module's: `webviewupdate` and
        // `hasSystemFeature` are answered by `core` **from the package table**,
        // which is the same table the provider install writes. A second factory
        // over the same state would be a second source of truth (and the
        // regression WV8 fixed in `registry.rs`).
        services: &[],
        replaces: &[],
        process_initializers: &[],
        features: &[],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        // The provision: one system app, installed before the guest boots.
        system_apps: &[PROVIDER_PACKAGE],
        requires: &[],
        requires_host_tools: &["acos-webview", "chrome-headless-shell"],
        // `stub`/`scripted` are deliberately absent (see the module docs).
        profiles: &[Profile::Real],
        budget: Budget {
            ram_mib: 4,
            startup_ms: 11,
        },
        // The provider package and the socket must exist before the guest runs.
        attach: AttachMode::AtLaunch,
        controls: &CONTROL_OPS,
        security: SecurityClass::HostTool,
    };
    &MANIFEST
}

/// Where the provider APK for this session is.
///
/// `config: {"provider_apk": …}` first (an explicit path wins), then
/// `ACOS_WEBVIEW_PROVIDER_APK`, then the image's own copy
/// ([`PROVIDER_APK_IMAGE_REL`] — what a fresh install ships and needs no
/// separate build step for), then the workspace build output
/// ([`PROVIDER_APK_REL`], the fixture). `None` when the path cannot even be
/// named, which a `real` attach refuses: a WebView without a provider package is
/// exactly the fake this framework forbids.
#[must_use]
pub fn provider_apk(config: &ModuleConfig) -> Option<PathBuf> {
    if let Some(path) = config.string(CONFIG_PROVIDER_APK).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os(PROVIDER_APK_ENV) {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    if let Some(image) = crate::workspace_path(PROVIDER_APK_IMAGE_REL) {
        return Some(image);
    }
    Some(crate::workspace_path(PROVIDER_APK_REL).unwrap_or_else(|| PathBuf::from(PROVIDER_APK_REL)))
}

/// The host APK a module's declared **system app** installs, for the launcher's
/// staging step (`crate::module::system_apks`).
///
/// `None` when `module`/`app` is not this module's.
#[must_use]
pub fn system_app_apk(module: &str, app: &str, config: &ModuleConfig) -> Option<PathBuf> {
    (module == MODULE_ID && app == PROVIDER_PACKAGE)
        .then(|| provider_apk(config))
        .flatten()
}

/// Does `specs` ask for this module's `real` profile?
///
/// The launcher reads this **before** the guest exists (it decides whether to
/// bind the gate socket and which system APKs to stage), so it is a pure
/// function of the specs the caller passed — the same specs `attach` later sees,
/// never a second source of truth.
#[must_use]
pub fn broker_requested(specs: &[crate::module::ModuleSpec]) -> bool {
    specs
        .iter()
        .any(|spec| spec.id == MODULE_ID && spec.profile == Profile::Real)
}

/// The `webview` module.
pub struct WebviewModule {
    profile: Profile,
    /// The provider APK this session installs (resolved at attach).
    provider_apk: Option<PathBuf>,
}

impl WebviewModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            provider_apk: None,
        }
    }

    /// The manifest (registry and tests).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }

    /// The provider APK this session installs, if attached.
    #[must_use]
    pub fn provider_apk(&self) -> Option<&PathBuf> {
        self.provider_apk.as_ref()
    }
}

impl Default for WebviewModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for WebviewModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        _host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        if !manifest_ref().supports(profile) {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "profile {profile} is not implemented: this module has `off` (Android's own \
                     \"no WebView provider\" path) and `real` (the `{PROVIDER_PACKAGE}` provider \
                     package plus the shared host Chromium). A scripted WebView would be a fake \
                     page, so there is none"
                ),
            });
        }
        let apk = provider_apk(config).ok_or_else(|| ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!(
                "the `real` profile needs the provider APK: pass \
                 `config: {{\"{CONFIG_PROVIDER_APK}\": \"<path>\"}}` or set {PROVIDER_APK_ENV} \
                 (the image ships {PROVIDER_APK_IMAGE_REL} from \
                 tools/extract-bootclasspath.sh; the fixture build is {PROVIDER_APK_REL})"
            ),
        })?;
        self.provider_apk = Some(apk);
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        self.provider_apk = None;
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, host: &mut ServiceHost, op: &str, _args: &Value) -> Result<Value> {
        if self.profile != Profile::Real {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: "module is off".to_string(),
            });
        }
        match op {
            "status" => {
                let apk = self.provider_apk.clone();
                let gate = host.webview_gate().cloned().unwrap_or_default();
                let socket = gate.socket.clone();
                let broker = gate.broker_socket.clone();
                Ok(json!({
                    "module": MODULE_ID,
                    "profile": self.profile.as_str(),
                    "provider": {
                        "package": PROVIDER_PACKAGE,
                        "apk": apk.as_ref().map(|p| p.display().to_string()),
                        "apk_present": apk.as_ref().is_some_and(|p| p.is_file()),
                        "guest_source_dir": crate::webview::PROVIDER_GUEST_SOURCE,
                    },
                    "gate": {
                        "socket": socket.as_ref().map(|p| p.display().to_string()),
                        "bound": socket.as_ref().is_some_and(|p| p.exists()),
                        "broker_socket": broker.as_ref().map(|p| p.display().to_string()),
                        "broker_listening": broker.as_ref().is_some_and(|p| p.exists()),
                        // ADR-0031/WVB: one Chromium per host, shared by every
                        // session; started on the first guest connect, never at
                        // launch.
                        "engine_scope": "one broker per host, started on the first connect",
                    },
                    "frames": {
                        // Measured, not chosen: this pinned engine cannot capture
                        // at deviceScaleFactor under `--enable-begin-frame-control`
                        // (WV7 `engine-capture-facts.txt`), so frames stay at the
                        // engine's CSS resolution and the wire reports their real
                        // size.
                        "resolution": "engine CSS pixels (device pixels / density)",
                        "reported": "the frame's real pixel size on the wire",
                    },
                    "budget": {
                        "per_session_ram_mib": manifest_ref().budget.ram_mib,
                        "fixed_host_ram_mib": 140,
                        "first_frame_ms": manifest_ref().budget.startup_ms,
                        "basis": "measured (WV6)",
                    },
                }))
            }
            other => Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "unknown control op {other:?}; this module has {}",
                    CONTROL_OPS
                        .iter()
                        .map(|op| op.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
        }
    }

    fn report(&self) -> ModuleReport {
        let mut report = ModuleReport::of(manifest_ref(), self.profile);
        if self.profile == Profile::Real {
            report.reason = Some(format!(
                "real WebView: provider package {PROVIDER_PACKAGE} installed from {}, one shared \
                 host Chromium behind the session's gate socket (started on the first connect)",
                self.provider_apk
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<unresolved>".to_string())
            ));
            report.details = Some(json!({
                "provider_package": PROVIDER_PACKAGE,
                "provider_apk": self.provider_apk.as_ref().map(|p| p.display().to_string()),
                "frame_resolution": "engine CSS pixels",
                "fixed_host_ram_mib": 140,
            }));
        }
        report
    }
}
