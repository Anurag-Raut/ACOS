//! `security.keystore` — session-private software `AndroidKeyStore` JCA provider.
//!
//! Android apps routinely use `KeyStore.getInstance("AndroidKeyStore")`,
//! `KeyGenerator.getInstance("AES", "AndroidKeyStore")`, `KeyPairGenerator`
//! (RSA/EC), and Jetpack Security (`EncryptedSharedPreferences` /
//! `MasterKeys`) for credential and token storage. ACOS runs `dalvikvm64`
//! without an Android Zygote or hardware keymaster daemon (`keystore2`).
//!
//! * `off` (the default): no `"AndroidKeyStore"` JCA `Provider` is registered.
//!   `Security.getProvider("AndroidKeyStore")` is `null` and
//!   `KeyStore.getInstance("AndroidKeyStore")` fails honestly with
//!   `KeyStoreException: AndroidKeyStore not found`. When guest code touches
//!   `"AndroidKeyStore"` while `off`, a lightweight diagnostic JCA service logs
//!   a clear `UNSUPPORTED_ANDROID_API` warning to `acos_logs` naming
//!   `security.keystore` and the launch attach call before throwing.
//! * `real`: installs ACOS's software-backed `AndroidKeyStore` JCA provider
//!   (`dev.acos.art.SoftwareKeyStore`) in the guest before any app code runs,
//!   persisting keys with `0600`/`0700` owner-only permissions under the
//!   session-private `/data/app-data/<package>/acos-keystore` directory.
//!
//! # Honest security model
//!
//! This module provides **software-backed** keys (`KeyInfo.getSecurityLevel()
//! == SECURITY_LEVEL_SOFTWARE`, `isInsideSecureHardware() == false`). It does
//! **not** provide TEE/StrongBox hardware backing, key attestation
//! (`UNSUPPORTED_ATTESTATION`), or biometric/user-authentication-bound keys.

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::host::ServiceHost;
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass,
};

/// The module id (`acos_launch {"services": [{"id": "security.keystore", "profile": "real"}]}`).
pub const MODULE_ID: &str = "security.keystore";

/// The Java JCA security provider name exposed to Android apps when attached.
pub const PROVIDER_NAME: &str = "AndroidKeyStore";

/// Guest-side directory suffix under `/data/app-data/<package>/`.
pub const KEYSTORE_DIR_SUFFIX: &str = "acos-keystore";

/// Legacy launcher environment seam enabled by `real` and suppressed by `off`.
pub const KEYSTORE_ENV: &str = "ACOS_ART_KEYSTORE";

const CONTROL_OPS: [ControlOp; 1] = [ControlOp {
    name: "status",
    args: "none — whether software AndroidKeyStore is active, its per-session storage path, \
           and its software security-level limits",
}];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "software-backed AndroidKeyStore JCA provider (SECURITY_LEVEL_SOFTWARE, \
                  non-hardware-backed): AES-256-GCM/CBC/CTR/ECB, HmacSHA256/SHA1, RSA/EC keys and \
                  self-signed X.509 chains with 0600/0700 session-private storage under \
                  /data/app-data/<pkg>/acos-keystore; no TEE/StrongBox, no attestation, no \
                  user-auth/biometric-bound keys (`off` fails honestly with AndroidKeyStore not \
                  found + diagnostic log breadcrumb)",
        services: &[],
        replaces: &[],
        process_initializers: &[],
        features: &[],
        providers: &[PROVIDER_NAME],
        broadcasts: &[],
        windows: &[],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &[],
        profiles: &[Profile::Real],
        budget: Budget {
            ram_mib: 1,
            startup_ms: 2,
        },
        // Zygote warms JCA providers before any Application.onCreate or
        // ContentProvider runs; a live session cannot retroactively install it.
        attach: AttachMode::AtLaunch,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

/// The last requested profile for [`MODULE_ID`] in `specs`, if explicitly listed.
///
/// The launcher checks this before spawning the guest so `{id: "security.keystore",
/// profile: "real"}` enables `ACOS_ART_KEYSTORE=on` and `{id: "security.keystore",
/// profile: "off"}` overrides any inherited host env var.
#[must_use]
pub fn requested_profile(specs: &[crate::module::ModuleSpec]) -> Option<Profile> {
    specs
        .iter()
        .rev()
        .find(|spec| spec.id == MODULE_ID)
        .map(|spec| spec.profile)
}

/// True when `specs` explicitly requests `security.keystore` at `real`.
#[must_use]
pub fn keystore_requested(specs: &[crate::module::ModuleSpec]) -> bool {
    matches!(requested_profile(specs), Some(Profile::Real))
}

/// True when `specs` explicitly requests `security.keystore` at `off`.
#[must_use]
pub fn explicit_off(specs: &[crate::module::ModuleSpec]) -> bool {
    matches!(requested_profile(specs), Some(Profile::Off))
}

/// The `security.keystore` module.
pub struct KeystoreModule {
    profile: Profile,
}

impl KeystoreModule {
    /// A detached module (`off`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
        }
    }

    /// The static manifest (registry and tests).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }
}

impl Default for KeystoreModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for KeystoreModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        _host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        let _ = config;
        if !manifest_ref().supports(profile) {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "profile {profile} is not implemented: this module has `off` (default: honest \
                     `AndroidKeyStore not found` failure with diagnostic log breadcrumb) and \
                     `real` (software-backed AndroidKeyStore, SECURITY_LEVEL_SOFTWARE)"
                ),
            });
        }
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, _host: &mut ServiceHost, op: &str, _args: &Value) -> Result<Value> {
        if self.profile != Profile::Real {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: "module is off".to_string(),
            });
        }
        match op {
            "status" => Ok(json!({
                "module": MODULE_ID,
                "profile": self.profile.as_str(),
                "attached": true,
                "provider": PROVIDER_NAME,
                "backing": "software",
                "security_level": "SECURITY_LEVEL_SOFTWARE",
                "inside_secure_hardware": false,
                "strongbox": false,
                "attestation": false,
                "user_auth": false,
                "storage": format!("/data/app-data/<package>/{KEYSTORE_DIR_SUFFIX}"),
                "limitations": [
                    "software-backed only (no TEE / no hardware keymaster / no StrongBox)",
                    "key attestation is refused explicitly (UNSUPPORTED_ATTESTATION)",
                    "user-authentication / biometric-bound keys are refused",
                    "session-private storage cleared when session ends"
                ],
            })),
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
                "software-backed {PROVIDER_NAME} (SECURITY_LEVEL_SOFTWARE, non-hardware-backed): \
                 installs JCA provider {PROVIDER_NAME} at launch with 0600/0700 session-private \
                 storage under /data/app-data/<pkg>/{KEYSTORE_DIR_SUFFIX}; no TEE/StrongBox, no \
                 attestation, no user-auth/biometric-bound keys"
            ));
            report.details = Some(json!({
                "provider": PROVIDER_NAME,
                "backing": "software",
                "security_level": "SECURITY_LEVEL_SOFTWARE",
                "inside_secure_hardware": false,
                "strongbox": false,
                "attestation": false,
                "user_auth": false,
                "storage": format!("/data/app-data/<package>/{KEYSTORE_DIR_SUFFIX}"),
            }));
        }
        report
    }
}

/// Record a `relaunch_required` capability prompt when guest code touches
/// `"AndroidKeyStore"` while `security.keystore` is `off`.
pub fn record_off_capability_prompt(
    prompts: &mut crate::capability_prompts::CapabilityPromptState,
    now_ms: u64,
) -> Option<crate::capability_prompts::CapabilityPrompt> {
    prompts.record_tombstone_hit(
        PROVIDER_NAME,
        MODULE_ID,
        AttachMode::AtLaunch,
        Profile::Real,
        now_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_prompts::{CapabilityPromptState, PromptOptionKind, PromptTrigger};

    #[test]
    fn off_keystore_hit_records_relaunch_required_capability_prompt() {
        let mut prompts = CapabilityPromptState::new();
        let prompt = record_off_capability_prompt(&mut prompts, 1_000)
            .expect("first hit records capability prompt");
        assert_eq!(prompt.trigger, PromptTrigger::ServiceTombstone);
        assert_eq!(prompt.request, "AndroidKeyStore (security.keystore)");
        assert_eq!(prompt.options.len(), 1);
        assert_eq!(prompt.options[0].kind, PromptOptionKind::RelaunchRequired);
        assert_eq!(prompt.options[0].modules.len(), 1);
        assert_eq!(prompt.options[0].modules[0].id, MODULE_ID);
        assert_eq!(prompt.options[0].modules[0].profile, "real");
        // Deduplicated within TOMBSTONE_DEDUP_MS.
        assert!(record_off_capability_prompt(&mut prompts, 1_500).is_none());
    }
}
