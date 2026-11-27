//! `system.launcher` module (ADR-0029): minimal guest Launcher system app.
//!
//! * `off` (default): no HOME activity is installed; pressing HOME (`key home` /
//!   `KEYCODE_HOME`) fails honestly (`HOME_UNAVAILABLE`). Zero cold-start cost
//!   on normal scenarios.
//! * `real`: stages `com.acos.launcher` (`fixtures/build/apps/launcher.apk`)
//!   before boot (`AttachMode::AtLaunch`). Pressing HOME starts or resumes
//!   `com.acos.launcher/.LauncherActivity` (`singleTask`), which queries
//!   `PackageManager` (`MAIN`/`LAUNCHER`) and launches tapped app rows.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::host::ServiceHost;
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass,
};

/// The module id (`services: [{id: system.launcher, profile: real}]`).
pub const MODULE_ID: &str = "system.launcher";
/// The guest Launcher package name.
pub const LAUNCHER_PACKAGE: &str = "com.acos.launcher";
/// `config: {"apk": ...}` override.
pub const CONFIG_APK: &str = "apk";
/// Environment override for the Launcher APK.
pub const APK_ENV: &str = "ACOS_LAUNCHER_APK";
/// Installed image-relative system-app APK (`tools/release/install.sh`).
pub const APK_IMAGE_REL: &str = ".cache/boot/android-34/system-apps/com.acos.launcher.apk";
/// Workspace-relative default build output (`tools/build/build-apps.sh`).
pub const APK_REL: &str = "fixtures/build/apps/launcher.apk";

const CONTROL_OPS: [ControlOp; 1] = [ControlOp {
    name: "status",
    args: "none — whether com.acos.launcher is staged for HOME and app launching",
}];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "guest Launcher system app (`com.acos.launcher`) answering ACTION_MAIN / CATEGORY_HOME and listing MAIN/LAUNCHER apps from PackageManager",
        services: &[],
        replaces: &[],
        process_initializers: &[],
        features: &[],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        system_apps: &[LAUNCHER_PACKAGE],
        requires: &[],
        requires_host_tools: &[],
        profiles: &[Profile::Real],
        budget: Budget {
            ram_mib: 1,
            startup_ms: 1,
        },
        attach: AttachMode::AtLaunch,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

/// Resolve the host APK path for `system.launcher`.
///
/// Order: `config: {"apk": ...}`, then `ACOS_LAUNCHER_APK`, then the installed
/// image path (`APK_IMAGE_REL` via `workspace_path`, checking `$ACOS_HOME`
/// first), then the workspace fixture build (`APK_REL`).
#[must_use]
pub fn launcher_apk(config: &ModuleConfig) -> PathBuf {
    if let Some(path) = config.string(CONFIG_APK).filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(path) = std::env::var_os(APK_ENV) {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    if let Some(image) = crate::workspace_path(APK_IMAGE_REL) {
        return image;
    }
    crate::workspace_path(APK_REL).unwrap_or_else(|| PathBuf::from(APK_REL))
}

/// Hook for `crate::modules::system_app_apk` / `crate::module::system_apks`.
#[must_use]
pub fn system_app_apk(module: &str, app: &str, config: &ModuleConfig) -> Option<PathBuf> {
    (module == MODULE_ID && app == LAUNCHER_PACKAGE).then(|| launcher_apk(config))
}

/// The `system.launcher` module.
pub struct LauncherModule {
    profile: Profile,
    apk: Option<PathBuf>,
}

impl LauncherModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            apk: None,
        }
    }

    /// The manifest (registry and tests).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }
}

impl Default for LauncherModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for LauncherModule {
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
                detail: format!("profile {profile} is not supported; use `real` or `off`"),
            });
        }
        self.apk = Some(launcher_apk(config));
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        self.apk = None;
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, _host: &mut ServiceHost, op: &str, _args: &Value) -> Result<Value> {
        if !self.profile.is_attached() {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("module is off, so {op:?} was not applied"),
            });
        }
        match op {
            "status" => Ok(json!({
                "attached": true,
                "package": LAUNCHER_PACKAGE,
                "apk": self.apk.as_ref().map(|p| p.display().to_string()),
            })),
            other => Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("unknown control op {other:?}; supported: status"),
            }),
        }
    }

    fn report(&self) -> ModuleReport {
        let mut report = ModuleReport::of(manifest_ref(), self.profile);
        if self.profile.is_attached() {
            report.reason = Some(format!(
                "real guest launcher `{LAUNCHER_PACKAGE}` staged for ACTION_MAIN/CATEGORY_HOME"
            ));
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_valid_and_at_launch() {
        manifest_ref().validate().expect("valid manifest");
        assert_eq!(manifest_ref().attach, AttachMode::AtLaunch);
        assert_eq!(manifest_ref().system_apps, &[LAUNCHER_PACKAGE]);
    }

    #[test]
    fn resolves_installed_prefix_before_checkout_and_respects_overrides() {
        let env = crate::test_env::EnvGuard::new(&["ACOS_HOME", APK_ENV]);
        let home = crate::test_env::TempDir::new("launcher-home");
        let installed_apk = home.path().join(APK_IMAGE_REL);
        std::fs::create_dir_all(installed_apk.parent().expect("parent"))
            .expect("create system-apps dir");
        std::fs::write(&installed_apk, b"fake-apk").expect("write installed launcher apk");

        env.set("ACOS_HOME", home.path());
        env.remove(APK_ENV);

        let default_cfg = ModuleConfig::default();
        assert_eq!(launcher_apk(&default_cfg), installed_apk);
        assert_eq!(
            system_app_apk(MODULE_ID, LAUNCHER_PACKAGE, &default_cfg),
            Some(installed_apk.clone())
        );
        assert_eq!(system_app_apk(MODULE_ID, "com.other", &default_cfg), None);

        let empty_cfg =
            ModuleConfig::from_json(MODULE_ID, &json!({ CONFIG_APK: "" })).expect("empty cfg");
        env.set(APK_ENV, "");
        assert_eq!(launcher_apk(&empty_cfg), installed_apk);

        let env_override = home.path().join("env-override.apk");
        env.set(APK_ENV, &env_override);
        assert_eq!(launcher_apk(&default_cfg), env_override);

        let cfg_override = home.path().join("cfg-override.apk");
        let cfg = ModuleConfig::from_json(
            MODULE_ID,
            &json!({ CONFIG_APK: cfg_override.to_string_lossy() }),
        )
        .expect("cfg");
        assert_eq!(launcher_apk(&cfg), cfg_override);
    }
}
