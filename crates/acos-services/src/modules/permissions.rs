//! `system.permissions` — the runtime-permission store, as a module (MODNOTIF2).
//!
//! Every option in the ADR-0029 amendment (system apps in the guest, host
//! system UI, or the service layer alone) needs the same thing: one table that
//! answers "is this permission granted?" for the framework's own check paths,
//! plus control ops an agent/scenario can drive. This module owns that table's
//! `IPermissionManager` face and takes the `permission` name over from `core`.
//!
//! ## What is real here, and what is not
//!
//! `real` is honest for the state and for the check paths:
//!
//! * `PackageManager.checkPermission` / `ActivityManager.checkPermission` (and
//!   `Context.checkSelfPermission`, which uses them) read the same
//!   [`crate::permissions::PermissionStore`] this module writes. `grant` in a
//!   scenario changes what the app sees; `revoke` changes it back.
//! * `getPermissionFlags` answers the real `FLAG_PERMISSION_USER_SET` /
//!   `FLAG_PERMISSION_USER_FIXED` bits, and
//!   `shouldShowRequestPermissionRationale` answers the documented contract
//!   ("true only after the user denied it and did not choose 'don't ask
//!   again'").
//! * `grantRuntimePermission` / `revokeRuntimePermission` /
//!   `updatePermissionFlags` succeed and change the table, exactly as the
//!   system service does.
//!
//! What is **not** here is the dialog. `Activity.requestPermissions` builds an
//! `Intent` addressed to the permission controller package
//! (`PackageManager.buildRequestPermissionsIntent`,
//! `base/core/java/android/content/pm/PackageManager.java:7049`) and starts it
//! for a result. ACOS installs no such system app yet (ADR-0029 amendment:
//! system UI comes from guest system apps, built on the multi-process work), so
//! the ATMS resolves that intent to a structured
//! `PERMISSION_UI_UNAVAILABLE` naming the system-app work, and to
//! `onRequestPermissionsResult`-equivalent denial nothing. It never grants
//! silently. A configuration whose permissions are all pre-granted resolves to
//! the same "already granted" state `GrantPermissionsActivity` would return,
//! and is reported as such.
//!
//! `control` ops: `grant {permission}`, `deny {permission, never_ask_again?}`,
//! `revoke {permission}`, `state` (every permission, its flags and rationale).
//!
//! Launch config: `{"grant": [...], "deny": [...],
//! "deny_never_ask_again": [...]}` — pre-grant or pre-deny before the app
//! starts, which is how the conformance fixture is driven from a scenario.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::host::{CallResult, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};
use crate::permissions::{PermissionStore, PERMISSION_GRANTED};
use crate::value::Value as WireValue;

/// The `ServiceManager` name this module takes over from `core`.
pub const SERVICE_NAME: &str = "permission";
/// The AIDL descriptor of the interface it serves.
pub const DESCRIPTOR: &str = "android.permission.IPermissionManager";
/// The module id.
pub const MODULE_ID: &str = "system.permissions";

/// The `ServiceManager` name -> descriptor pairs this module serves.
///
/// `permissionmgr` is `core`'s second name for the same descriptor
/// (`registry::install` aliases it), so it follows this module's registration
/// and core's on detach without a second entry here.
pub const SERVICES: &[ServiceProvide] = &[ServiceProvide {
    name: SERVICE_NAME,
    descriptor: DESCRIPTOR,
}];

const CONTROL_OPS: [ControlOp; 4] = [
    ControlOp {
        name: "grant",
        args: "{permission} — grant a runtime permission (the app's checkSelfPermission answers granted)",
    },
    ControlOp {
        name: "deny",
        args: "{permission, never_ask_again?} — deny it; `never_ask_again: true` also sets FLAG_PERMISSION_USER_FIXED",
    },
    ControlOp {
        name: "revoke",
        args: "{permission} — clear the runtime state, back to the install-time answer",
    },
    ControlOp {
        name: "state",
        args: "none — every permission with granted/never_ask_again/rationale/flags, the requests and the mutations",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "runtime permissions: one shared store behind both checkPermission paths, flags, rationale, grant/deny/revoke (no dialog: the controller system app is not built yet)",
        services: SERVICES,
        replaces: &[SERVICE_NAME],
        process_initializers: &[],
        features: &[],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &[],
        // `real` for the state and every check path; the dialog is the
        // `system_apps` work named in the report, not a profile lie.
        profiles: &[Profile::Real],
        budget: Budget {
            ram_mib: 1,
            startup_ms: 2,
        },
        attach: AttachMode::Hot,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

/// A string field from a control-op/launch-config object.
fn string_arg(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("{key:?} is a required string"),
        })
}

/// The module.
pub struct PermissionsModule {
    profile: Profile,
    store: Option<Arc<Mutex<PermissionStore>>>,
    /// The pre-grant/pre-deny config applied at attach, so the report can name
    /// what the scenario configured.
    configured: Option<Value>,
}

impl PermissionsModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            store: None,
            configured: None,
        }
    }

    /// The manifest (tests and the registry).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }
}

impl Default for PermissionsModule {
    fn default() -> Self {
        Self::new()
    }
}

/// The `IPermissionManager` face, over the shared store.
struct PermissionsService {
    store: Arc<Mutex<PermissionStore>>,
}

impl PermissionsService {
    fn store(&self) -> std::sync::MutexGuard<'_, PermissionStore> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ServiceObject for PermissionsService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        args: &[WireValue],
    ) -> Result<CallResult> {
        if interface != DESCRIPTOR {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let string = |index: usize| -> String {
            args.get(index)
                .and_then(WireValue::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let int =
            |index: usize| -> i32 { args.get(index).and_then(WireValue::as_int).unwrap_or(0) };
        let reply = match method.name.as_str() {
            // `getPermissionFlags(String packageName, String permissionName, int userId)`
            "getPermissionFlags" => {
                let permission = string(1);
                vec![WireValue::Int(self.store().flags(&permission))]
            }
            // `shouldShowRequestPermissionRationale(String packageName, String permissionName, int userId)`
            "shouldShowRequestPermissionRationale" => {
                let permission = string(1);
                vec![WireValue::Bool(
                    self.store().should_show_rationale(&permission),
                )]
            }
            // `checkPermission(String packageName, String permissionName, int userId)`
            "checkPermission" => {
                let permission = string(1);
                vec![WireValue::Int(self.store().check(&permission))]
            }
            // `isPermissionRevokedByPolicy(...)`: no device policy on ACOS.
            "isPermissionRevokedByPolicy" => vec![WireValue::Bool(false)],
            "getAllowlistedRestrictedPermissions"
            | "getAutoRevokeExemptionRequestedPackages"
            | "getAutoRevokeExemptionGrantedPackages"
            | "getEnabledNotificationListenerPackages" => {
                vec![WireValue::List(Some(Vec::new()))]
            }
            "isAutoRevokeExempted" => vec![WireValue::Bool(false)],
            // `grantRuntimePermission(String packageName, String permissionName, int userId)`
            "grantRuntimePermission" => {
                let permission = string(1);
                self.store().grant(&permission);
                Vec::new()
            }
            // `revokeRuntimePermission(String packageName, String permissionName, int userId, String reason)`
            "revokeRuntimePermission" => {
                let permission = string(1);
                self.store().revoke(&permission);
                Vec::new()
            }
            // `updatePermissionFlags(String packageName, String permissionName,
            //  int flagMask, int flagValues, int userId)` and the legacy
            // 3-arg form. Only the two flags this store models are applied;
            // a mask bit ACOS does not model (`POLICY_FIXED`, `SYSTEM_FIXED`)
            // is refused instead of silently dropped.
            "updatePermissionFlags" | "updatePermissionFlagsForAllApps" => {
                let permission = string(1);
                let mask = int(2);
                let values = int(3);
                let modelled = crate::permissions::FLAG_PERMISSION_USER_SET
                    | crate::permissions::FLAG_PERMISSION_USER_FIXED;
                if mask & !modelled != 0 {
                    return Err(ServiceError::UnsupportedAndroidApi {
                        interface: DESCRIPTOR.to_string(),
                        code: method.code,
                        method: format!(
                            "updatePermissionFlags mask {mask:#x}: only USER_SET/USER_FIXED are modelled"
                        ),
                    });
                }
                let mut store = self.store();
                if mask & crate::permissions::FLAG_PERMISSION_USER_SET != 0 {
                    store.set_user_set(
                        &permission,
                        values & crate::permissions::FLAG_PERMISSION_USER_SET != 0,
                    );
                }
                if mask & crate::permissions::FLAG_PERMISSION_USER_FIXED != 0 {
                    store.set_user_fixed(
                        &permission,
                        values & crate::permissions::FLAG_PERMISSION_USER_FIXED != 0,
                    );
                }
                Vec::new()
            }
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

/// Apply a launch-config `grant`/`deny`/`deny_never_ask_again` list.
fn apply_config(store: &mut PermissionStore, config: &ModuleConfig) -> Result<Value> {
    let mut granted: Vec<String> = Vec::new();
    let mut denied: Vec<String> = Vec::new();
    let mut never: Vec<String> = Vec::new();
    for permission in config.string_array("grant") {
        store.grant(&permission);
        granted.push(permission);
    }
    for permission in config.string_array("deny") {
        store.deny(&permission, false);
        denied.push(permission);
    }
    for permission in config.string_array("deny_never_ask_again") {
        store.deny(&permission, true);
        never.push(permission);
    }
    Ok(json!({
        "granted": granted,
        "denied": denied,
        "denied_never_ask_again": never,
    }))
}

impl Module for PermissionsModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        if profile != Profile::Real {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "{profile} is not implemented: the runtime-permission store is the real implementation; use `real`"
                ),
            });
        }
        // The session's table, when `core` published one, so `checkPermission`
        // (ATMS/PMS) and this module cannot disagree. A bare host (the
        // conformance kit) gets a fresh table, published for the same reason.
        let store = match host.permission_store() {
            Some(store) => store,
            None => {
                let store = Arc::new(Mutex::new(PermissionStore::new()));
                host.set_permission_store(Arc::clone(&store));
                store
            }
        };
        self.configured = Some(apply_config(
            &mut store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            config,
        )?);
        // Registered through the factory path (`IPermissionManager` is in the
        // decoded `acos-aidl` tables), so the decoded argument list reaches
        // `call` and the reply is written through the same codec the framework's
        // proxy reads. The descriptor is the one `core` registered, which is what
        // makes the takeover an *overwrite* `end_scope` can restore.
        let service_store = Arc::clone(&store);
        host.register(SERVICE_NAME, DESCRIPTOR, move |_wiring| {
            Box::new(PermissionsService {
                store: Arc::clone(&service_store),
            }) as Box<dyn ServiceObject>
        });
        self.store = Some(store);
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        // `end_scope` restores core's `IPermissionManager` registration and the
        // host's previous store slot. The table itself is *not* dropped: it is
        // the session's permission state, and `core`'s check paths keep reading
        // it, which is what "detach restores the core answer" means.
        self.store = None;
        self.configured = None;
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, _host: &mut ServiceHost, op: &str, args: &Value) -> Result<Value> {
        let Some(store) = self.store.as_ref() else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("module is off, so {op:?} was not applied"),
            });
        };
        let mut store = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match op {
            "grant" => {
                let permission = string_arg(args, "permission")?;
                store.grant(&permission);
                Ok(json!({"granted": permission, "state": store.check(&permission)}))
            }
            "deny" => {
                let permission = string_arg(args, "permission")?;
                let never = args
                    .get("never_ask_again")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                store.deny(&permission, never);
                Ok(json!({
                    "denied": permission,
                    "never_ask_again": never,
                    "state": store.check(&permission),
                    "rationale": store.should_show_rationale(&permission),
                }))
            }
            "revoke" => {
                let permission = string_arg(args, "permission")?;
                store.revoke(&permission);
                Ok(json!({"revoked": permission, "state": store.check(&permission)}))
            }
            "state" => {
                let mut state = store.to_json();
                state["attached"] = json!(true);
                state["configuration"] = self.configured.clone().unwrap_or(Value::Null);
                Ok(state)
            }
            other => Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "unknown control op {other:?}; this module implements {}",
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
        if self.profile.is_attached() {
            let granted = self
                .store
                .as_ref()
                .map(|store| {
                    store
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .to_json()
                })
                .unwrap_or(Value::Null);
            let granted_count = granted
                .get("permissions")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter(|row| row.get("granted").and_then(Value::as_bool) == Some(true))
                        .count()
                })
                .unwrap_or(0);
            report.reason = Some(format!(
                "real runtime-permission store: {granted_count} granted permission(s) seen by both checkPermission paths; the request dialog needs the permission-controller system app (PERMISSION_UI_UNAVAILABLE)"
            ));
        }
        report
    }
}

/// `PERMISSION_GRANTED` is re-exported so a caller reading this module's JSON
/// can compare against the same constant the services use.
pub const GRANTED: i32 = PERMISSION_GRANTED;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_is_valid_and_declares_the_takeover() {
        manifest_ref().validate().expect("valid manifest");
        assert_eq!(manifest_ref().replaces, &["permission"]);
    }
}
