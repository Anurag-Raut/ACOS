//! `accessibility` — the real `AccessibilityManagerService` state (ADR-0029).
//!
//! This module owns the `accessibility` `ServiceManager` name. Attaching it
//! with `real` is the programmatic equivalent of switching an accessibility
//! service on in Settings:
//!
//! ```text
//! services: [{id: "accessibility", profile: "real",
//!             config: {enable: ["com.gaten/.GatenAccessibilityService"]}}]
//! ```
//!
//! It writes exactly the two secure settings a phone records —
//! `Settings.Secure.ACCESSIBILITY_ENABLED` and
//! `ENABLED_ACCESSIBILITY_SERVICES` (colon-separated, as
//! `AccessibilityManagerService.setAccessibilityEnabled` writes them) — into the
//! session's settings provider, and it answers the `IAccessibilityManager`
//! transactions an app's real `AccessibilityManager` makes:
//!
//! | transaction | answer |
//! |---|---|
//! | `addClient` | a client id plus the oneway `setState(int)` the app needs to make `isEnabled()` true |
//! | `getInstalledAccessibilityServiceList` | the configured services as `AccessibilityServiceInfo` (the install happens after module attach) |
//! | `getEnabledAccessibilityServiceList` | the same list when the module is enabled, empty otherwise |
//! | `sendAccessibilityEvent` | recorded (type + time, bounded log) — decoded by `codec::read_args_override` because the event parcel has no codec |
//! | `addAccessibilityInteractionConnection` | recorded (window, connection, package) |
//! | `removeAccessibilityInteractionConnection` | removes the record |
//! | `interrupt` | recorded |
//!
//! # Honest limits (this is the first slice)
//!
//! The service is **not bound** yet. AOSP's `AccessibilityManagerService` binds
//! the app's `AccessibilityService` and calls
//! `IAccessibilityServiceClient.init(IAccessibilityServiceConnection, ...)`;
//! ACOS's ART path has no system-initiated bind (`IApplicationThread.scheduleBindService`
//! delivery plus `IActivityManager.publishService` plus the connection surface
//! and the interaction-connection round trip). The module says so in its report
//! (`service_bound: false`) instead of pretending `onServiceConnected` ran.
//! `takeScreenshot`, `dispatchGesture` and `performGlobalAction` are therefore
//! not reachable from a service yet either; the module refuses them with a
//! structured error naming the missing bind, never a silent success.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::host::{ServiceHost, ServiceObject};
use crate::misc::{A11yService, A11yState};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};

/// Module id.
pub const MODULE_ID: &str = "accessibility";

const CONTROL_OPS: [ControlOp; 3] = [
    ControlOp {
        name: "list_events",
        args: "{\"limit\": N} — the recent a11y events (newest last, default 50)",
    },
    ControlOp {
        name: "global_action",
        args: "{\"action\": \"BACK\"|\"HOME\"|\"RECENTS\"|\"NOTIFICATIONS\"} — performed through the session's input path",
    },
    ControlOp {
        name: "status",
        args: "none — the module's full honest state (enabled, connections, event counts)",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "0.1.0",
        summary: "AccessibilityManagerService state: enable services via secure settings, client state, event/interaction records (service binding not yet)",
        services: &[ServiceProvide {
            name: "accessibility",
            descriptor: "android.view.accessibility.IAccessibilityManager",
        }],
        features: &[],
        process_initializers: &[],
        providers: &[],
        broadcasts: &[],
        windows: &["TYPE_ACCESSIBILITY_OVERLAY"],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &[],
        profiles: &[Profile::Real],
        budget: Budget {
            ram_mib: 2,
            startup_ms: 10,
        },
        attach: AttachMode::Hot,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

/// The module.
pub struct AccessibilityModule {
    profile: Profile,
    state: Option<Arc<Mutex<A11yState>>>,
}

impl AccessibilityModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            state: None,
        }
    }

    /// The manifest.
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }
}

impl Default for AccessibilityModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AccessibilityModule {
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
                    "the accessibility module has only the `real` profile (asked for {profile})"
                ),
            });
        }
        let services = config.string_array("enable");
        // The service object is constructed by `core`'s install (shared state).
        // Attaching the module only configures it; no second registration is
        // made, so `detach` cannot remove a service the session booted with.
        // In a session, `core` installs the shared service (it also owns the
        // settings provider the module writes). The conformance kit builds a
        // host with only the module, so attach registers the service itself;
        // the registry's attach scope rolls it back on detach.
        let object = match host.ensure("android.view.accessibility.IAccessibilityManager") {
            Ok(object) => object,
            Err(ServiceError::UnsupportedAndroidApi { .. }) => {
                let state = Arc::new(Mutex::new(A11yState::new()));
                host.register(
                    "accessibility",
                    "android.view.accessibility.IAccessibilityManager",
                    move |_wiring| {
                        Box::new(A11yService::new(Arc::clone(&state))) as Box<dyn ServiceObject>
                    },
                );
                host.ensure("android.view.accessibility.IAccessibilityManager")?
            }
            Err(other) => return Err(other),
        };
        let state = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<A11yService>())
            .map(|service| service.state())
            .ok_or_else(|| ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: "host has no A11yService implementation; core did not install one".into(),
            })?;
        {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.enable_services(&services)?;
            if let Some(touch) = config.get("touch_exploration").and_then(Value::as_bool) {
                if let Some(settings) = &state.settings {
                    settings
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .secure
                        .insert(
                            "touch_exploration_enabled".into(),
                            if touch { "1" } else { "0" }.into(),
                        );
                }
            }
        }
        self.state = Some(state);
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        if let Some(state) = &self.state {
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .disable();
        }
        self.state = None;
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, _host: &mut ServiceHost, op: &str, args: &Value) -> Result<Value> {
        let Some(state) = &self.state else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("control {op:?} needs an attached module"),
            });
        };
        match op {
            "status" => Ok(state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .to_json()),
            "list_events" => {
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(50)
                    .min(256) as usize;
                let state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let start = state.events.len().saturating_sub(limit);
                Ok(json!({
                    "count": state.events.len(),
                    "events": state.events[start..].iter().map(|event| json!({
                        "type": event.event_type,
                        "time": event.event_time,
                    })).collect::<Vec<_>>(),
                }))
            }
            "global_action" => {
                let action = args
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_ascii_uppercase();
                match action.as_str() {
                    "BACK" => Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: "BACK is the session's own `back` step; the accessibility module has no input handle, so this op does not claim to have performed it".into(),
                    }),
                    "HOME" | "RECENTS" => Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!(
                            "{action} needs a launcher/recents system app, which this session does not have; unsupported (not a silent no-op)"
                        ),
                    }),
                    "NOTIFICATIONS" => Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: "the notifications module owns the shade; attach it and open the shade through it"
                            .into(),
                    }),
                    other => Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!(
                            "unknown global action {other:?}; expected BACK, HOME, RECENTS or NOTIFICATIONS"
                        ),
                    }),
                }
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
        if let Some(state) = &self.state {
            let state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut details = state.to_json();
            if let Some(object) = details.as_object_mut() {
                object.insert(
                    "binding".into(),
                    Value::String(if state.bound_service.is_some() {
                        "bound: the app published its IAccessibilityServiceClient and the system delivered init(IAccessibilityServiceConnection, connectionId, windowToken), so onServiceConnected ran"
                            .into()
                    } else if state.pending_bind.is_some() {
                        "bind in flight: scheduleCreateService + scheduleBindService were delivered; waiting for IActivityManager.publishService"
                            .into()
                    } else if !state.enabled {
                        "accessibility is off, so no service is bound".into()
                    } else {
                        "no bind yet: it starts when the app's AccessibilityManager calls addClient and attachApplication named its IApplicationThread"
                            .into()
                    }),
                );
            }
            report.details = Some(details);
            if self.profile.is_attached() {
                report.reason = Some(
                    "real: settings, client state, service lists, event and interaction records, system-initiated bind (scheduleCreateService + scheduleBindService -> publishService -> IAccessibilityServiceClient.init); the interaction surface (getWindows, node queries, gestures, screenshot) is not implemented yet"
                        .into(),
                );
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::ServiceHost;
    use crate::host::ServiceObject;
    use crate::module::{ModuleRegistry, ModuleSpec};
    use crate::provider::SettingsState;

    fn host_with_a11y() -> ServiceHost {
        ServiceHost::new()
    }

    fn a11y_state(host: &mut ServiceHost) -> Arc<Mutex<A11yState>> {
        host.ensure("android.view.accessibility.IAccessibilityManager")
            .expect("service")
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<A11yService>())
            .expect("A11yService")
            .state()
    }

    #[test]
    fn attaching_writes_the_two_secure_settings() {
        let mut host = host_with_a11y();
        let mut registry = ModuleRegistry::standard();
        registry
            .attach_session(
                &mut host,
                &[ModuleSpec::parse(
                    "accessibility:real,{\"enable\":[\"com.gaten/.GatenAccessibilityService\"]}",
                )
                .expect("spec")],
            )
            .expect("attach");
        let state = registry
            .reports()
            .into_iter()
            .find(|r| r.id == MODULE_ID)
            .expect("report");
        let details = state.details.expect("details");
        assert_eq!(details["enabled"], true);
        assert_eq!(
            details["enabled_services"][0],
            "com.gaten/.GatenAccessibilityService"
        );
        // The provider table is the same one `Settings.Secure` reads.
        let settings = a11y_state(&mut host)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .settings
            .clone()
            .expect("settings handle");
        let settings = settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            settings.secure.get("enabled_accessibility_services"),
            Some(&"com.gaten/.GatenAccessibilityService".to_string())
        );
        assert_eq!(
            settings.secure.get("accessibility_enabled"),
            Some(&"1".to_string())
        );
    }

    #[test]
    fn detach_clears_the_settings_and_the_state() {
        let mut host = host_with_a11y();
        let mut registry = ModuleRegistry::standard();
        registry
            .attach_session(
                &mut host,
                &[ModuleSpec::parse(
                    "accessibility:real,{\"enable\":[\"com.gaten/.GatenAccessibilityService\"]}",
                )
                .expect("spec")],
            )
            .expect("attach");
        registry.detach(&mut host, MODULE_ID).expect("detach");
        let report = registry
            .reports()
            .into_iter()
            .find(|r| r.id == MODULE_ID)
            .expect("report");
        assert!(!report.attached);
        assert!(report.reason.as_deref().is_some_and(|r| r.contains("off")));
    }

    #[test]
    fn a_bare_package_is_refused() {
        let mut host = host_with_a11y();
        let mut registry = ModuleRegistry::standard();
        let err = registry
            .attach_session(
                &mut host,
                &[
                    ModuleSpec::parse("accessibility:real,{\"enable\":[\"com.gaten\"]}")
                        .expect("spec"),
                ],
            )
            .expect_err("must refuse");
        assert!(format!("{err}").contains("package/.Class"));
    }

    #[test]
    fn an_empty_enable_attaches_with_accessibility_off() {
        let mut host = host_with_a11y();
        let mut registry = ModuleRegistry::standard();
        registry
            .attach_session(
                &mut host,
                &[ModuleSpec::parse("accessibility:real").expect("spec")],
            )
            .expect("attach");
        let report = registry
            .reports()
            .into_iter()
            .find(|r| r.id == MODULE_ID)
            .expect("report");
        let details = report.details.expect("details");
        assert_eq!(details["enabled"], false);
        assert_eq!(details["service_bound"], false);
    }

    #[test]
    fn the_list_transactions_are_answered_not_unsupported() {
        let mut host = host_with_a11y();
        let mut registry = ModuleRegistry::standard();
        registry.attach_session(&mut host, &[]).expect("core");
        let state = a11y_state(&mut host);
        {
            let mut state = state.lock().unwrap();
            state
                .enable_services(&["com.gaten/.GatenAccessibilityService".into()])
                .unwrap();
            // The installed list is the package manager's answer (MODA11Y2),
            // so the test installs the component in the in-process PMS.
            let pms = state.pms.clone().expect("package manager wired");
            let mut pms = pms.lock().unwrap();
            pms.app.package_name = "com.gaten".into();
            pms.package.package_name = "com.gaten".into();
            pms.services.push(crate::pms::ServiceInfo {
                name: "com.gaten.GatenAccessibilityService".into(),
                package_name: "com.gaten".into(),
                process_name: "com.gaten".into(),
                permission: "android.permission.BIND_ACCESSIBILITY_SERVICE".into(),
                exported: true,
                flags: 0,
                foreground_service_type: 0,
                meta_data: Vec::new(),
            });
        }
        let method = acos_aidl::lookup("android.view.accessibility.IAccessibilityManager", 5)
            .expect("table row");
        let mut service = A11yService::new(state);
        let reply = service
            .call(
                "android.view.accessibility.IAccessibilityManager",
                method,
                &[crate::value::Value::Int(0)],
            )
            .expect("must be answered");
        match &reply.reply[0] {
            crate::value::Value::List(Some(items)) => assert_eq!(items.len(), 1),
            other => panic!("expected one service info, got {other:?}"),
        }
    }

    #[test]
    fn an_event_reaches_the_record_through_the_transaction_path() {
        let mut host = host_with_a11y();
        let mut registry = ModuleRegistry::standard();
        registry
            .attach_session(
                &mut host,
                &[ModuleSpec::parse(
                    "accessibility:real,{\"enable\":[\"com.gaten/.GatenAccessibilityService\"]}",
                )
                .expect("spec")],
            )
            .expect("attach");
        let binder = host
            .check_service("accessibility")
            .unwrap()
            .expect("binder");
        let mut data = acos_parcel::Parcel::new();
        data.write_interface_token("android.view.accessibility.IAccessibilityManager")
            .unwrap();
        data.write_int32(1); // isSealed
        data.write_int32(1); // TYPE_VIEW_CLICKED
        data.write_int32(0); // userId
        data.set_data_position(0);
        let mut reply = acos_parcel::Parcel::new();
        host.transact(binder, 2, &mut data, &mut reply, 1)
            .expect("oneway accepted");
        let report = registry
            .reports()
            .into_iter()
            .find(|r| r.id == MODULE_ID)
            .expect("report");
        let details = report.details.expect("details");
        assert_eq!(details["event_count"], 1);
        assert_eq!(details["event_types"][0]["type"], 1);
    }

    #[test]
    fn adding_a_client_returns_the_state_in_the_high_word() {
        let mut host = host_with_a11y();
        let mut registry = ModuleRegistry::standard();
        registry
            .attach_session(
                &mut host,
                &[ModuleSpec::parse(
                    "accessibility:real,{\"enable\":[\"com.gaten/.GatenAccessibilityService\"]}",
                )
                .expect("spec")],
            )
            .expect("attach");
        let state = a11y_state(&mut host);
        let mut service = A11yService::new(state);
        let method = acos_aidl::lookup("android.view.accessibility.IAccessibilityManager", 3)
            .expect("table row");
        let reply = service
            .call(
                "android.view.accessibility.IAccessibilityManager",
                method,
                &[
                    crate::value::Value::Binder(Some(acos_parcel::BinderId(0x1234))),
                    crate::value::Value::Int(0),
                ],
            )
            .expect("addClient");
        let crate::value::Value::Long(packed) = reply.reply[0] else {
            panic!("expected a long");
        };
        assert_eq!(packed >> 32, 1, "state flags are the high word");
    }

    #[test]
    fn settings_default_is_off() {
        let state = SettingsState::new();
        assert_eq!(
            state.secure.get("accessibility_enabled"),
            Some(&"0".to_string())
        );
    }
}
