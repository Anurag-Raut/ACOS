//! `system.settings` module (ADR-0029): minimal guest Settings stub app.
//!
//! * `off` (default): no Settings activity is installed; `android.settings.*`
//!   intents fail honestly with `ActivityNotFoundException` (`START_NOT_FOUND`).
//!   Zero cold-start cost on normal scenarios.
//! * `real`: stages `com.android.settings` (`fixtures/build/apps/settings.apk`)
//!   before boot (`AttachMode::AtLaunch`), handling `ACTION_SETTINGS`,
//!   `ACTION_APPLICATION_DETAILS_SETTINGS` (`package:` URI), and
//!   `ACTION_WIFI_SETTINGS`. Undeclared settings actions continue to throw
//!   `ActivityNotFoundException`.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::host::ServiceHost;
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass,
};

/// The module id (`services: [{id: system.settings, profile: real}]`).
pub const MODULE_ID: &str = "system.settings";
/// The guest Settings package name.
pub const SETTINGS_PACKAGE: &str = "com.android.settings";
/// `config: {"apk": ...}` override.
pub const CONFIG_APK: &str = "apk";
/// Environment override for the Settings APK.
pub const APK_ENV: &str = "ACOS_SETTINGS_APK";
/// Workspace-relative default build output (`tools/build/build-apps.sh`).
pub const APK_REL: &str = "fixtures/build/apps/settings.apk";

const CONTROL_OPS: [ControlOp; 1] = [ControlOp {
    name: "status",
    args: "none — whether com.android.settings is staged for settings intents",
}];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "guest Settings stub system app (`com.android.settings`) answering ACTION_SETTINGS, ACTION_APPLICATION_DETAILS_SETTINGS, and ACTION_WIFI_SETTINGS",
        services: &[],
        replaces: &[],
        process_initializers: &[],
        features: &[],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        system_apps: &[SETTINGS_PACKAGE],
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

/// Resolve the host APK path for `system.settings`.
#[must_use]
pub fn settings_apk(config: &ModuleConfig) -> PathBuf {
    if let Some(path) = config.string(CONFIG_APK).filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(path) = std::env::var_os(APK_ENV) {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    crate::workspace_path(APK_REL).unwrap_or_else(|| PathBuf::from(APK_REL))
}

/// Hook for `crate::modules::system_app_apk` / `crate::module::system_apks`.
#[must_use]
pub fn system_app_apk(module: &str, app: &str, config: &ModuleConfig) -> Option<PathBuf> {
    (module == MODULE_ID && app == SETTINGS_PACKAGE).then(|| settings_apk(config))
}

/// The `system.settings` module.
pub struct SettingsModule {
    profile: Profile,
    apk: Option<PathBuf>,
}

impl SettingsModule {
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

impl Default for SettingsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SettingsModule {
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
        self.apk = Some(settings_apk(config));
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
                "package": SETTINGS_PACKAGE,
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
                "real guest Settings stub `{SETTINGS_PACKAGE}` staged for SETTINGS / APPLICATION_DETAILS_SETTINGS / WIFI_SETTINGS"
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
        assert_eq!(manifest_ref().system_apps, &[SETTINGS_PACKAGE]);
    }
}
