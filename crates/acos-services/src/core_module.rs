//! The `core` module: today's service registrations, as a module (ADR-0029 item 1).
//!
//! `core` is what `registry::install_with_handles` has always installed — AMS,
//! ATMS, PMS, the window manager session, the compositor client, display, IME,
//! clipboard, the declared services, the extras and the `settings` provider. It
//! is attached at `real` for every session and is the only module that has a
//! `real` profile: it *is* the implementation, not a stand-in.
//!
//! The point of wrapping it is proof, not architecture: the conformance kit
//! (`module::conformance`) runs attach → detach twice against `core` and requires
//! the process to come back to its baseline, so "detach leaves no registration,
//! thread, fd or socket" is a measured property of the existing set and every
//! later module is held to the same bar.
//!
//! One honest limit: the `settings` provider binder is cached in a
//! process-global ([`crate::provider::set_settings_provider_binder`]) because the
//! runtime's `getContentProvider` bridge reads it. Detaching `core` clears the
//! host's registration (the kit measures that), but that global keeps the last
//! value it was given until the next `core` attach. A session never detaches
//! `core` (it is `at-launch` only), so this is a test-visible detail, recorded
//! here rather than hidden.

use std::sync::OnceLock;

use serde_json::Value;

use crate::error::{Result, ServiceError};
use crate::host::ServiceHost;
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};

/// The `core` module.
pub struct CoreModule {
    profile: Profile,
    /// The compositor/input handles `attach` published, taken once by the
    /// launch path ([`Module::take_session_handles`]). Detach drops them.
    handles: Option<crate::registry::LiveHandles>,
}

impl CoreModule {
    /// A detached `core` module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            handles: None,
        }
    }

    /// The manifest, built once from the one service table
    /// ([`crate::registry::NAMES`]) so the two cannot drift.
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        static MANIFEST: OnceLock<ModuleManifest> = OnceLock::new();
        MANIFEST.get_or_init(|| {
            let services: &'static [ServiceProvide] = Box::leak(
                crate::registry::NAMES
                    .iter()
                    .filter(|(name, _)| !crate::registry::AUX_BACKED_NAMES.contains(name))
                    .filter(|(name, _)| !FILLED_BY_MODULE.contains(name))
                    .map(|(name, descriptor)| ServiceProvide {
                        name,
                        descriptor,
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            );
            ModuleManifest {
                id: "core",
                version: "1.0.0",
                summary: "the Android system services every session boots with (AMS, ATMS, PMS, WMS, IME, clipboard, settings, display, extras)",
                services,
                providers: &["settings"],
                broadcasts: &[],
                windows: &[],
                system_apps: &[],
                requires: &[],
                requires_host_tools: &[],
                profiles: &[Profile::Real],
                // Measured: the whole service state is part of the launcher
                // process (D-M2-INVENTORY: ~45-55 MiB anon/session including the
                // guest); the module's own share is the state machines' tables.
                budget: Budget {
                    ram_mib: 16,
                    startup_ms: 250,
                },
                attach: AttachMode::AtLaunch,
                controls: &CONTROLS,
                security: SecurityClass::InProcess,
            }
        })
    }
}

/// `core` accepts no control ops: it is the implementation, and a control that
/// changed AMS/PMS behaviour behind the agent's back would be a fake session.
const CONTROLS: [ControlOp; 0] = [];

/// Service names `core` **publishes** but does not claim in its manifest, because
/// the behaviour behind them belongs to a module.
///
/// * `sensorservice` — `core` registers the node with no feed (the stock answer
///   of a device without sensors) for two reasons: a `null` name costs
///   `SystemSensorManager.nativeCreate` two 5 s libbinder retry loops (W26 §1),
///   and an app resolves the `ISensorServer` proxy once and keeps it, so a hot
///   attach must reuse the same binder. The `sensors` module installs a feed into
///   that node and owns the name in the registry/capabilities, which is why it is
///   filtered out of this manifest (the registry refuses two owners).
pub const FILLED_BY_MODULE: &[&str] = &["sensorservice"];

impl Default for CoreModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CoreModule {
    fn manifest(&self) -> &'static ModuleManifest {
        Self::manifest_ref()
    }

    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        _config: &ModuleConfig,
    ) -> Result<()> {
        if profile != Profile::Real {
            return Err(ServiceError::ModuleRejected {
                module: "core",
                detail: format!(
                    "core is the implementation and has only the `real` profile (asked for {profile})"
                ),
            });
        }
        // The exact call sessions made before modules existed. The handles it
        // returns (the input/compositor state a live session drives) are
        // published through `take_session_handles` so a launch path can attach
        // modules and still drive the same C1/input state.
        self.handles = Some(crate::registry::install_with_handles(host));
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        // Every registration was made inside the registry's attach scope, which
        // is rolled back after this returns (ADR-0029 item 2). Nothing of
        // `core` runs outside the host, so there is no thread or fd to stop.
        self.profile = Profile::Off;
        self.handles = None;
        Ok(())
    }

    fn take_session_handles(&mut self) -> Option<crate::registry::LiveHandles> {
        self.handles.take()
    }

    fn control(&mut self, op: &str, _args: &Value) -> Result<Value> {
        Err(ServiceError::ModuleRejected {
            module: "core",
            detail: format!(
                "core has no control operations (asked for {op:?}); it is the real implementation, not a scriptable stand-in"
            ),
        })
    }

    fn report(&self) -> ModuleReport {
        ModuleReport::of(Self::manifest_ref(), self.profile)
    }
}
