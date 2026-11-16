//! Service modules: attachable Android building blocks (ADR-0029).
//!
//! Today's service set is one **module** (`core`) that is always attached. A
//! module owns a set of Android system pieces (ServiceManager names, content
//! providers, broadcasts, windows, system apps), carries a manifest that says
//! what it provides, what it requires, which profiles it supports and what it
//! costs, and is attached to / detached from a session through one interface:
//!
//! ```text
//! attach(host, profile, config) -> Result<()>
//! detach(host)                  -> Result<()>
//! control(op, args)             -> Result<Value>
//! report()                      -> ModuleReport
//! ```
//!
//! `attach` sits on the existing [`ServiceHost::register`] seam. `detach` uses
//! [`ServiceHost::begin_scope`]/[`ServiceHost::end_scope`], so *every*
//! registration the module made (services, aux binders, dynamic nodes, aliases)
//! is rolled back as a unit: a detached module leaves no registration, no
//! dynamic node and no queued delivery behind. Threads and fds a module opened
//! are its own responsibility and are measured by the conformance kit
//! ([`conformance`]).
//!
//! ## Tombstones
//!
//! A name that a real phone publishes but this build has no module for must
//! never answer a silent `null` that an app reads as a working phone. The
//! [`TOMBSTONES`] table names those names, the module that will own them, and —
//! per name — what the AOSP client code does with a **null** binder today. Where
//! a present-but-throwing binder is safe (AOSP only stores the proxy during
//! initialization) the name answers with a tombstone that always reports
//! [`ServiceError::ModuleUnavailable`]: `UNSUPPORTED_ANDROID_API` naming the
//! module and the attach call. Where AOSP needs the missing service to stay
//! `null` (it builds a working "disabled" manager from it, or the client source
//! is not in the offline AOSP tree and a throwing binder is unverified) the name
//! keeps answering `null` and is reported as `off` with that reason.
//!
//! [`ServiceManager`]: https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-14.0.0_r1/core/java/android/os/ServiceManager.java

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::host::{RegistrationScope, ServiceHost, ServiceObject};

/// The four profiles a module can be attached with (ADR-0029 item 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Profile {
    /// Not attached: the framework takes its real "missing" path and every reply
    /// says the module is off.
    Off,
    /// Fixed, documented answers (no behaviour, but no lie either).
    Stub,
    /// The agent drives the state (`control` ops).
    Scripted,
    /// A real implementation.
    Real,
}

impl Profile {
    /// Every profile, weakest first.
    pub const ALL: [Profile; 4] = [Profile::Off, Profile::Stub, Profile::Scripted, Profile::Real];

    /// The wire/CLI spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Off => "off",
            Profile::Stub => "stub",
            Profile::Scripted => "scripted",
            Profile::Real => "real",
        }
    }

    /// Parse the wire/CLI spelling (`--service wifi:scripted`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Profile::ALL
            .into_iter()
            .find(|profile| profile.as_str() == value)
    }

    /// `true` when the profile leaves the module attached.
    #[must_use]
    pub fn is_attached(self) -> bool {
        self != Profile::Off
    }
}

impl core::fmt::Display for Profile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// When a module can be attached (ADR-0029 item 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachMode {
    /// Only while the session is being created (`acos_launch {services: [...]}`).
    AtLaunch,
    /// At launch **and** on a live session (`acos_services {op: "attach"}`).
    Hot,
}

impl AttachMode {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AttachMode::AtLaunch => "at-launch",
            AttachMode::Hot => "hot",
        }
    }
}

/// The security class of a module: what it needs in order to exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityClass {
    /// Pure in-process service state, no host access.
    InProcess,
    /// Needs a host tool (a relay, the shared Chromium) through the host broker.
    HostTool,
    /// Needs elevated capability inside the session sandbox.
    Privileged,
}

impl SecurityClass {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SecurityClass::InProcess => "in-process",
            SecurityClass::HostTool => "host-tool",
            SecurityClass::Privileged => "privileged",
        }
    }
}

/// A module's declared RAM and startup budget (ADR-0029 item 1).
///
/// The conformance kit checks the RAM figure against the measured attach cost
/// and the density run; a module whose budget is exceeded fails the kit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// Resident memory the attached module may add, in MiB.
    pub ram_mib: u32,
    /// Attach-to-first-answer startup time the module may add, in ms.
    pub startup_ms: u32,
}

/// One `ServiceManager` name a module provides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceProvide {
    /// `ServiceManager` name (`vibrator_manager`).
    pub name: &'static str,
    /// AIDL interface descriptor the client proxy writes in its interface token.
    pub descriptor: &'static str,
}

/// A control operation a module accepts (`acos_service_ctl {module, op, args}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlOp {
    /// Operation name (`set_online`).
    pub name: &'static str,
    /// One-line description of its arguments, for the generated docs.
    pub args: &'static str,
}

/// Everything a module promises about itself (ADR-0029 item 1).
///
/// A manifest is static data: one row in the registry, never per-session. The
/// per-session state (which profile is attached) lives in the module object and
/// in [`ModuleReport`].
#[derive(Debug)]
pub struct ModuleManifest {
    /// Stable module id (`net.wifi`), used in API calls and verdicts.
    pub id: &'static str,
    /// Manifest version of this module implementation.
    pub version: &'static str,
    /// One line for the generated capability/limits tables.
    pub summary: &'static str,
    /// `ServiceManager` names (with their AIDL descriptors) the module provides.
    pub services: &'static [ServiceProvide],
    /// Content-provider authorities the module provides.
    pub providers: &'static [&'static str],
    /// Broadcasts the module starts or answers.
    pub broadcasts: &'static [&'static str],
    /// Window types (layer stack) the module owns.
    pub windows: &'static [&'static str],
    /// System apps the module installs.
    pub system_apps: &'static [&'static str],
    /// Module ids that must be attached first.
    pub requires: &'static [&'static str],
    /// Host tools the module needs (`pasta`, `chrome-headless-shell`).
    pub requires_host_tools: &'static [&'static str],
    /// Profiles this module implements. `off` is always possible and is not
    /// listed; `stub`/`scripted`/`real` are listed only when implemented.
    pub profiles: &'static [Profile],
    /// Declared cost.
    pub budget: Budget,
    /// Whether it can be attached to a live session.
    pub attach: AttachMode,
    /// Control operations (`scripted` state).
    pub controls: &'static [ControlOp],
    /// What it needs from the sandbox.
    pub security: SecurityClass,
}

impl ModuleManifest {
    /// Check the manifest against the rules the conformance kit enforces
    /// (ADR-0029 item 7: "manifest valid").
    ///
    /// Every rule here is a test that would otherwise only fail much later, at
    /// attach time or in an agent's verdict: an empty id, a duplicate service
    /// name, a `real` profile a module does not implement, a budget of zero.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.id.is_empty() {
            return Err("module id is empty".into());
        }
        if !self
            .id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        {
            return Err(format!(
                "module id {:?} must be lowercase ascii letters/digits/'.'/'-'",
                self.id
            ));
        }
        if self.version.is_empty() {
            return Err(format!("module {:?}: version is empty", self.id));
        }
        if self.summary.is_empty() {
            return Err(format!("module {:?}: summary is empty", self.id));
        }
        if self.services.is_empty()
            && self.providers.is_empty()
            && self.broadcasts.is_empty()
            && self.windows.is_empty()
            && self.system_apps.is_empty()
        {
            return Err(format!(
                "module {:?} provides nothing (no service, provider, broadcast, window or system app)",
                self.id
            ));
        }
        let mut seen_names: Vec<&str> = Vec::new();
        for service in self.services {
            if service.name.is_empty() || service.descriptor.is_empty() {
                return Err(format!(
                    "module {:?}: service entry {service:?} needs a name and a descriptor",
                    self.id
                ));
            }
            if seen_names.contains(&service.name) {
                return Err(format!(
                    "module {:?}: service name {:?} is listed twice",
                    self.id, service.name
                ));
            }
            seen_names.push(service.name);
        }
        let mut seen_profiles: Vec<Profile> = Vec::new();
        for profile in self.profiles {
            if *profile == Profile::Off {
                return Err(format!(
                    "module {:?}: `off` is implicit and must not be listed in profiles",
                    self.id
                ));
            }
            if seen_profiles.contains(profile) {
                return Err(format!(
                    "module {:?}: profile {profile} is listed twice",
                    self.id
                ));
            }
            seen_profiles.push(*profile);
        }
        if self.budget.ram_mib == 0 {
            return Err(format!(
                "module {:?}: budget.ram_mib must be > 0 (a module that cannot say what it costs cannot be scheduled)",
                self.id
            ));
        }
        if self.budget.startup_ms == 0 {
            return Err(format!(
                "module {:?}: budget.startup_ms must be > 0",
                self.id
            ));
        }
        let mut seen_ops: Vec<&str> = Vec::new();
        for op in self.controls {
            if op.name.is_empty() {
                return Err(format!("module {:?}: a control op has no name", self.id));
            }
            if seen_ops.contains(&op.name) {
                return Err(format!(
                    "module {:?}: control op {:?} is listed twice",
                    self.id, op.name
                ));
            }
            seen_ops.push(op.name);
        }
        if self.controls.is_empty() && self.profiles.contains(&Profile::Scripted) {
            return Err(format!(
                "module {:?}: declares the scripted profile but no control ops",
                self.id
            ));
        }
        Ok(())
    }

    /// The profiles this module accepts, `off` included.
    #[must_use]
    pub fn profiles_with_off(&self) -> Vec<Profile> {
        let mut profiles = vec![Profile::Off];
        profiles.extend_from_slice(self.profiles);
        profiles
    }

    /// Whether `profile` is implemented.
    #[must_use]
    pub fn supports(&self, profile: Profile) -> bool {
        profile == Profile::Off || self.profiles.contains(&profile)
    }

    /// The generated one-row capability description.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "version": self.version,
            "summary": self.summary,
            "attach": self.attach.as_str(),
            "profiles": self.profiles_with_off().iter().map(|p| p.as_str()).collect::<Vec<_>>(),
            "provides": {
                "services": self.services.iter().map(|s| json!({
                    "name": s.name,
                    "descriptor": s.descriptor,
                })).collect::<Vec<_>>(),
                "providers": self.providers,
                "broadcasts": self.broadcasts,
                "windows": self.windows,
                "system_apps": self.system_apps,
            },
            "requires": self.requires,
            "requires_host_tools": self.requires_host_tools,
            "budget": {"ram_mib": self.budget.ram_mib, "startup_ms": self.budget.startup_ms},
            "security": self.security.as_str(),
            "controls": self.controls.iter().map(|op| json!({
                "op": op.name,
                "args": op.args,
            })).collect::<Vec<_>>(),
        })
    }
}

/// One `services:` entry from a launch request or scenario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleSpec {
    /// Module id.
    pub id: String,
    /// Requested profile (`off` detaches).
    pub profile: Profile,
    /// Module-specific configuration (launch config, scripted state).
    pub config: ModuleConfig,
}

impl ModuleSpec {
    /// A spec at `profile` with no config.
    #[must_use]
    pub fn new(id: impl Into<String>, profile: Profile) -> Self {
        Self {
            id: id.into(),
            profile,
            config: ModuleConfig::default(),
        }
    }

    /// Parse the CLI spelling `id[:profile]` (`vibrator:scripted`).
    ///
    /// An unknown profile is an error, never a silent fallback: `wifi:rela`
    /// must not attach something the agent did not ask for.
    pub fn parse(text: &str) -> Result<Self> {
        let (id, profile) = match text.split_once(':') {
            Some((id, profile)) => (id, profile),
            None => (text, "real"),
        };
        if id.is_empty() {
            return Err(ServiceError::ModuleRejected {
                module: "<launch>",
                detail: format!("service spec {text:?} has no module id"),
            });
        }
        let Some(profile) = Profile::parse(profile) else {
            return Err(ServiceError::ModuleRejected {
                module: "<launch>",
                detail: format!(
                    "service spec {text:?} names unknown profile {profile:?}; use one of {}",
                    Profile::ALL
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        };
        Ok(Self::new(id, profile))
    }

    /// The launch-reply / verdict row for this request.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "profile": self.profile.as_str(),
            "config": self.config.to_json(),
        })
    }
}

/// Per-session module configuration (`services: [{id, profile, config}]`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleConfig {
    values: serde_json::Map<String, Value>,
}

impl ModuleConfig {
    /// Wrap a JSON object.
    ///
    /// A non-object is refused (`ModuleRejected`) rather than ignored: an agent
    /// that passes `config: "enable"` instead of `config: {enable: [...]}` must
    /// learn that before the module silently attaches with defaults.
    pub fn from_json(module: &'static str, value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(Self::default()),
            Value::Object(map) => Ok(Self {
                values: map.clone(),
            }),
            other => Err(ServiceError::ModuleRejected {
                module,
                detail: format!("config must be a JSON object, got {other}"),
            }),
        }
    }

    /// A raw value.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    /// A string value.
    #[must_use]
    pub fn string(&self, key: &str) -> Option<&str> {
        self.values.get(key).and_then(Value::as_str)
    }

    /// An array of strings (`{enable: ["com.app/.Service"]}`).
    #[must_use]
    pub fn string_array(&self, key: &str) -> Vec<String> {
        self.values
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The JSON form, for the launch reply and the verdict.
    #[must_use]
    pub fn to_json(&self) -> Value {
        Value::Object(self.values.clone())
    }

    /// True when nothing was configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// The honest state of one module in a session (ADR-0029 item 5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleReport {
    /// Module id (must equal the manifest's, checked by the kit).
    pub id: &'static str,
    /// Version from the manifest.
    pub version: &'static str,
    /// The profile it is attached with, `None` when detached.
    pub profile: Option<Profile>,
    /// Whether it is attached.
    pub attached: bool,
    /// Why it is not attached, or what is degraded while it is. Never empty for
    /// an unattached module: a report that cannot say why is not honest.
    pub reason: Option<String>,
    /// `ServiceManager` names it provides.
    pub services: Vec<&'static str>,
    /// Declared cost.
    pub budget: Budget,
    /// Attach mode.
    pub attach: AttachMode,
    /// Module ids it requires.
    pub requires: &'static [&'static str],
    /// The control operations it accepts.
    pub controls: Vec<&'static str>,
}

impl ModuleReport {
    /// Build a report from a manifest and the current profile.
    #[must_use]
    pub fn of(manifest: &'static ModuleManifest, profile: Profile) -> Self {
        let attached = profile.is_attached();
        Self {
            id: manifest.id,
            version: manifest.version,
            profile: attached.then_some(profile),
            attached,
            reason: if attached {
                None
            } else {
                Some(format!(
                    "module is off; attach it: {}",
                    attach_call(manifest.id, default_profile(manifest))
                ))
            },
            services: manifest.services.iter().map(|s| s.name).collect(),
            budget: manifest.budget,
            attach: manifest.attach,
            requires: manifest.requires,
            controls: manifest.controls.iter().map(|op| op.name).collect(),
        }
    }

    /// The JSON form (launch reply, `acos_services`, verdict.json).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "version": self.version,
            "attached": self.attached,
            "profile": self.profile.map(Profile::as_str),
            "reason": self.reason,
            "attach": self.attach.as_str(),
            "services": self.services,
            "requires": self.requires,
            "controls": self.controls,
            "budget": {"ram_mib": self.budget.ram_mib, "startup_ms": self.budget.startup_ms},
        })
    }
}

/// The default attach profile a module is offered with when the caller does not
/// name one: the strongest profile it implements.
#[must_use]
pub fn default_profile(manifest: &ModuleManifest) -> Profile {
    manifest
        .profiles
        .iter()
        .copied()
        .max()
        .unwrap_or(Profile::Stub)
}

/// The exact call that attaches a module, embedded in every tombstone.
#[must_use]
pub fn attach_call(id: &str, profile: Profile) -> String {
    format!("acos_launch {{\"services\": [{{\"id\": \"{id}\", \"profile\": \"{profile}\"}}]}}")
}

/// A module: one unit of Android system pieces with a manifest.
///
/// One object per session (a [`ModuleRegistry`] is per session), so the
/// attached profile can live in the module itself.
pub trait Module: Send {
    /// The static manifest.
    fn manifest(&self) -> &'static ModuleManifest;

    /// Attach to `host` with `profile` and `config`.
    ///
    /// Called inside a [`ServiceHost::begin_scope`] scope; the registry rolls
    /// the scope back when this returns `Err`, so a failed attach registers
    /// nothing.
    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()>;

    /// Detach from `host`.
    ///
    /// The registry rolls back every registration made since `attach` whether or
    /// not this removed them itself, so the contract is: stop whatever runs, and
    /// leave the host consistent. Removing registrations here too is allowed
    /// (and preferred when a module must drop a registration *before* a thread
    /// stops).
    fn detach(&mut self, host: &mut ServiceHost) -> Result<()>;

    /// Drive scripted state (`acos_service_ctl`). Must refuse when detached.
    fn control(&mut self, op: &str, args: &Value) -> Result<Value>;

    /// The honest state (ADR-0029 item 5).
    fn report(&self) -> ModuleReport;
}

/// The AOSP behaviour a tombstone name is decided by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AospReaction {
    /// AOSPs client stores the binder and only calls it later: a
    /// present-but-throwing binder cannot break initialization, and today's
    /// `null` either throws at `getSystemService` or turns the call into a
    /// silent no-op. So the name gets a tombstone.
    Tombstone,
    /// AOSP builds *working* behaviour out of `null` (a disabled manager), or
    /// its client code is not in the offline AOSP tree so a throwing binder is
    /// unverified. Keep `null`; report the name as `off`.
    Null,
}

/// One name a real phone publishes and this build has no module for.
#[derive(Clone, Copy, Debug)]
pub struct Tombstone {
    /// `ServiceManager` name (`vibrator_manager`).
    pub name: &'static str,
    /// AIDL interface descriptor the client proxy uses.
    pub descriptor: &'static str,
    /// Module id that will own it.
    pub module: &'static str,
    /// What is missing, in one line, for the agent.
    pub reason: &'static str,
    /// The decision for this name.
    pub reaction: AospReaction,
    /// Where the decision comes from (AOSP file + line/function).
    pub evidence: &'static str,
}

impl Tombstone {
    /// The tombstone body: the error every transaction on the name answers.
    #[must_use]
    pub fn error(&self, interface: &str) -> ServiceError {
        ServiceError::ModuleUnavailable {
            service: self.name.to_string(),
            interface: interface.to_string(),
            module: self.module,
            attach: attach_call(self.module, Profile::Real),
        }
    }
}

/// The gap list of `docs/STATE_AND_PLAN_2026-11-15.md` §6, name by name, with
/// the AOSP evidence for what happens when the name answers `null` today.
///
/// Order follows the gap list. Every entry appears in `acos_capabilities` either
/// as a tombstone or as `off` with its reason (ADR-0029 item 3/6).
pub const TOMBSTONES: &[Tombstone] = &[
    Tombstone {
        name: "vibrator_manager",
        descriptor: "android.os.IVibratorManagerService",
        module: "feedback.vibrator",
        reason: "no vibrator hardware model; `Vibrator.vibrate()` is a silent no-op today",
        reaction: AospReaction::Tombstone,
        evidence: "base/core/java/android/os/SystemVibratorManager.java:57-63 stores the proxy in its constructor (no transaction at init); SystemVibrator.java:70-78 logs `Failed to retrieve vibrator ids; no vibrator manager service` and returns a silent no-op for a null manager",
    },
    Tombstone {
        name: "vibrator",
        descriptor: "android.os.IVibrator",
        module: "feedback.vibrator",
        reason: "no vibrator hardware model",
        reaction: AospReaction::Null,
        evidence: "SystemVibrator does not look `vibrator` up itself; it resolves `vibrator_manager` (SystemVibrator.java:61) and does everything through it, so the tombstone there is the one that matters and this name stays null as on API-34 (no client reads it)",
    },
    Tombstone {
        name: "usb",
        descriptor: "android.hardware.usb.IUsbManager",
        module: "hw.usb",
        reason: "no USB host/device model",
        reaction: AospReaction::Tombstone,
        evidence: "SystemServiceRegistry.java:735-740 fetches with getServiceOrThrow, so a null name already throws ServiceNotFoundException out of getSystemService(UsbManager.class); a tombstone moves that to a structured error at first use and cannot break init",
    },
    Tombstone {
        name: "midi",
        descriptor: "android.media.midi.IMidiManager",
        module: "hw.midi",
        reason: "no MIDI device model",
        reaction: AospReaction::Tombstone,
        evidence: "SystemServiceRegistry.java:1077-1083 getServiceOrThrow + `new MidiManager(proxy)`; null already throws at getSystemService",
    },
    Tombstone {
        name: "launcherapps",
        descriptor: "android.content.pm.ILauncherApps",
        module: "system.launcher",
        reason: "no launcher and no launcher-apps service",
        reaction: AospReaction::Tombstone,
        evidence: "LauncherApps.java:672-675 stores the proxy (`this(context, Stub.asInterface(getService(...)))`) and never transacts in the constructor; a null binder is a silent NPE later",
    },
    Tombstone {
        name: "usagestats",
        descriptor: "android.app.usage.IUsageStatsManager",
        module: "system.usage",
        reason: "no usage-stats service (nothing records app usage)",
        reaction: AospReaction::Tombstone,
        evidence: "SystemServiceRegistry.java:1022-1028 getServiceOrThrow + UsageStatsManager.java:448-451 stores the proxy; null already throws at getSystemService",
    },
    Tombstone {
        name: "dropbox",
        descriptor: "android.os.IDropBoxManagerService",
        module: "system.dropbox",
        reason: "no DropBox service",
        reaction: AospReaction::Tombstone,
        evidence: "SystemServiceRegistry.java:488-494 getServiceOrThrow + DropBoxManager.java:299-302 stores the proxy; null already throws at getSystemService",
    },
    Tombstone {
        name: "device_policy",
        descriptor: "android.app.admin.IDevicePolicyManager",
        module: "system.dpm",
        reason: "no device-policy manager (no managed device/profile)",
        reaction: AospReaction::Tombstone,
        evidence: "SystemServiceRegistry.java:462-466 getServiceOrThrow + DevicePolicyManager.java:246-252 stores the proxy; null already throws at getSystemService",
    },
    Tombstone {
        name: "batterystats",
        descriptor: "com.android.internal.app.IBatteryStats",
        module: "system.battery",
        reason: "no battery-stats service",
        reaction: AospReaction::Tombstone,
        evidence: "SystemServiceRegistry.java:1426-1436 getServiceOrThrow; null already throws at getSystemService",
    },
    Tombstone {
        name: "fingerprint",
        descriptor: "android.hardware.fingerprint.IFingerprintService",
        module: "hw.biometric",
        reason: "no fingerprint/biometric HAL",
        reaction: AospReaction::Tombstone,
        evidence: "SystemServiceRegistry.java:916-927 getServiceOrThrow for targetSdk >= O; null already throws at getSystemService",
    },
    Tombstone {
        name: "auth",
        descriptor: "android.hardware.biometrics.IAuthService",
        module: "hw.biometric",
        reason: "no biometric authentication service",
        reaction: AospReaction::Tombstone,
        evidence: "SystemServiceRegistry.java:958-967 getServiceOrThrow(Context.AUTH_SERVICE) for BiometricManager; null already throws at getSystemService",
    },
    Tombstone {
        name: "print",
        descriptor: "android.print.IPrintManager",
        module: "system.print",
        reason: "no print service",
        reaction: AospReaction::Tombstone,
        evidence: "SystemServiceRegistry.java:857-868 only looks the name up when PMS reports FEATURE_PRINTING; ACOS reports the feature absent, so a tombstone is only reached if the feature is turned on and then answers the structured error instead of a null proxy",
    },
    Tombstone {
        name: "country_detector",
        descriptor: "android.location.ICountryDetector",
        module: "geo.country",
        reason: "no country detector",
        reaction: AospReaction::Tombstone,
        evidence: "SystemServiceRegistry.java:454-459 getServiceOrThrow; null already throws at getSystemService",
    },
    Tombstone {
        name: "wallpaper",
        descriptor: "android.app.IWallpaperManager",
        module: "system.wallpaper",
        reason: "no wallpaper service",
        reaction: AospReaction::Null,
        evidence: "SystemServiceRegistry.java:774-793 turns a null binder into `DisabledWallpaperManager` (config_enableWallpaperService false) — a working, honest \"no wallpaper\" manager; a throwing binder would replace that graceful path with a hard error",
    },
    Tombstone {
        name: "wifi",
        descriptor: "android.net.wifi.IWifiManager",
        module: "net.wifi",
        reason: "no Wi-Fi stack (the network module owns real internet, ADR-0030)",
        reaction: AospReaction::Null,
        evidence: "WifiManager's client source lives under packages/modules/Wifi, which is not in the offline AOSP cache (.cache/aosp is framework/base only), so a present-but-throwing binder is unverified; null is today's behaviour and cannot crash init",
    },
    Tombstone {
        name: "phone",
        descriptor: "com.android.internal.telephony.ITelephony",
        module: "net.telephony",
        reason: "no telephony stack",
        reaction: AospReaction::Null,
        evidence: "TelephonyManager is not in the offline AOSP cache; null is today's behaviour",
    },
    Tombstone {
        name: "bluetooth",
        descriptor: "android.bluetooth.IBluetoothManager",
        module: "hw.bluetooth",
        reason: "no Bluetooth stack",
        reaction: AospReaction::Null,
        evidence: "BluetoothAdapter/BluetoothManager are not in the offline AOSP cache (packages/modules/Bluetooth); null is today's behaviour",
    },
    Tombstone {
        name: "bluetooth_manager",
        descriptor: "android.bluetooth.IBluetoothManager",
        module: "hw.bluetooth",
        reason: "no Bluetooth stack",
        reaction: AospReaction::Null,
        evidence: "as `bluetooth`: client source not cached",
    },
    Tombstone {
        name: "nfc",
        descriptor: "android.nfc.INfcAdapter",
        module: "hw.nfc",
        reason: "no NFC stack",
        reaction: AospReaction::Null,
        evidence: "NfcAdapter is not in the offline AOSP cache (packages/apps/Nfc); null is today's behaviour",
    },
    Tombstone {
        name: "media.camera",
        descriptor: "android.hardware.ICameraService",
        module: "hw.camera",
        reason: "no camera HAL",
        reaction: AospReaction::Null,
        evidence: "CameraManager reaches the camera service from native code (CameraManagerGlobal), not through the Java ServiceManager path; the module must provide the HAL seam, so a Java tombstone would be bypassed",
    },
    Tombstone {
        name: "media_session",
        descriptor: "android.media.session.ISessionManager",
        module: "media.session",
        reason: "no media-session service",
        reaction: AospReaction::Null,
        evidence: "MediaSessionManager's client source is not in the offline AOSP cache; null is today's behaviour",
    },
    Tombstone {
        name: "media_router",
        descriptor: "android.media.IMediaRouterService",
        module: "media.session",
        reason: "no media-router service",
        reaction: AospReaction::Null,
        evidence: "MediaRouter static initialization is not in the offline AOSP cache; a throwing binder there is unverified",
    },
    Tombstone {
        name: "search",
        descriptor: "android.app.ISearchManager",
        module: "system.search",
        reason: "no search service",
        reaction: AospReaction::Null,
        evidence: "SearchManager's constructor fetches the service itself (app/SearchManager.java has no `public SearchManager(Context)`); its init path is unverified, so null stays",
    },
    Tombstone {
        name: "role",
        descriptor: "android.app.role.IRoleManager",
        module: "system.role",
        reason: "no role manager",
        reaction: AospReaction::Null,
        evidence: "RoleManager is not in the offline AOSP cache; null is today's behaviour",
    },
    Tombstone {
        name: "carrier_config",
        descriptor: "com.android.internal.telephony.ICarrierConfigLoader",
        module: "net.telephony",
        reason: "no telephony stack",
        reaction: AospReaction::Null,
        evidence: "CarrierConfigManager is not in the offline AOSP cache; null is today's behaviour",
    },
    Tombstone {
        name: "webviewupdate",
        descriptor: "android.webkit.IWebViewUpdateService",
        module: "web.webview",
        reason: "no WebView provider yet (ADR-0031)",
        reaction: AospReaction::Null,
        evidence: "reachable only through WebViewFactory, which the package manager reports absent; null is today's behaviour",
    },
];

/// The tombstone for a `ServiceManager` name, when the gap list has one.
#[must_use]
pub fn tombstone_for(name: &str) -> Option<&'static Tombstone> {
    TOMBSTONES.iter().find(|t| t.name == name)
}

/// A service object that answers every transaction with the tombstone error.
struct TombstoneService {
    tombstone: &'static Tombstone,
}

impl ServiceObject for TombstoneService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<crate::host::CallResult> {
        Err(self.tombstone.error(interface)).map_err(|error| {
            let _ = method;
            error
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        _code: u32,
        _data: &mut acos_parcel::Parcel,
        _reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        Err(self.tombstone.error(interface))
    }
}

/// Register a tombstone for `tombstone` on `host` and return its binder.
///
/// The node is dynamic (it has no decoded table and no factory) and named, so
/// `IServiceManager.checkService(name)` answers it on the first try. Every
/// transaction on it answers [`ServiceError::ModuleUnavailable`].
pub fn install_tombstone(
    host: &mut ServiceHost,
    tombstone: &'static Tombstone,
) -> acos_parcel::BinderId {
    host.register_dynamic_named(
        tombstone.name,
        tombstone.descriptor,
        Box::new(TombstoneService { tombstone }),
    )
}

/// One attached module in a session.
struct Entry {
    module: Box<dyn Module>,
    profile: Profile,
    scope: Option<RegistrationScope>,
}

/// The per-session module registry: the one source of truth for which modules
/// exist, what they provide and what is attached (ADR-0029 items 1, 6).
pub struct ModuleRegistry {
    entries: Vec<Entry>,
    /// Runtime-added module ids, for diagnostics.
    attached_order: Vec<&'static str>,
    /// Service name -> module id, from the manifests.
    providers: HashMap<&'static str, &'static str>,
    /// Tombstones registered by [`Self::install_tombstones`].
    tombstoned: Vec<&'static str>,
}

impl ModuleRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            attached_order: Vec::new(),
            providers: HashMap::new(),
            tombstoned: Vec::new(),
        }
    }

    /// The standard registry: `core` (always attached) plus the modules this
    /// build ships.
    #[must_use]
    pub fn standard() -> Self {
        let mut registry = Self::new();
        for module in [
            Box::new(crate::core_module::CoreModule::new()) as Box<dyn Module>,
            Box::new(crate::modules::vibrator::VibratorModule::new()),
        ] {
            if let Err(error) = registry.add(module) {
                panic!("standard module set is invalid: {error}");
            }
        }
        registry
    }

    /// Add a module. Returns the manifest-validation error when the manifest is
    /// invalid; a module with an invalid manifest is never installed.
    pub fn add(&mut self, module: Box<dyn Module>) -> std::result::Result<(), String> {
        let manifest = module.manifest();
        manifest.validate()?;
        for service in manifest.services {
            if let Some(owner) = self.providers.insert(service.name, manifest.id) {
                return Err(format!(
                    "module {:?} provides {:?}, which module {owner:?} already provides",
                    manifest.id, service.name
                ));
            }
        }
        self.entries.push(Entry {
            module,
            profile: Profile::Off,
            scope: None,
        });
        Ok(())
    }

    /// Every manifest, in registration order.
    #[must_use]
    pub fn manifests(&self) -> Vec<&'static ModuleManifest> {
        self.entries
            .iter()
            .map(|entry| entry.module.manifest())
            .collect()
    }

    /// The manifest for `id`.
    #[must_use]
    pub fn manifest(&self, id: &str) -> Option<&'static ModuleManifest> {
        self.entries
            .iter()
            .find(|entry| entry.module.manifest().id == id)
            .map(|entry| entry.module.manifest())
    }

    /// The module id that provides a `ServiceManager` name.
    #[must_use]
    pub fn module_of_service(&self, name: &str) -> Option<&'static str> {
        self.providers.get(name).copied()
    }

    /// The current profile of `id` (`Off` when it does not exist).
    #[must_use]
    pub fn profile(&self, id: &str) -> Option<Profile> {
        self.entries
            .iter()
            .find(|entry| entry.module.manifest().id == id)
            .map(|entry| entry.profile)
    }

    /// True when the module exists.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.manifest(id).is_some()
    }

    /// Every id, in registration order.
    #[must_use]
    pub fn ids(&self) -> Vec<&'static str> {
        self.entries
            .iter()
            .map(|entry| entry.module.manifest().id)
            .collect()
    }

    /// Attach `id` with `profile`.
    ///
    /// * `off` detaches (a requested `off` is remembered so the reply can say it
    ///   was asked for).
    /// * An unknown id, a profile the module does not implement, an unmet
    ///   `requires` entry or a failed `attach` is an error and leaves the host
    ///   exactly as it was.
    pub fn attach(
        &mut self,
        host: &mut ServiceHost,
        id: &str,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.module.manifest().id == id)
            .ok_or_else(|| ServiceError::ModuleRejected {
                module: "<launch>",
                detail: format!(
                    "unknown module {id:?}; known modules: {}",
                    self.ids().join(", ")
                ),
            })?;
        {
            let manifest = self.entries[index].module.manifest();
            if !manifest.supports(profile) {
                return Err(ServiceError::ModuleRejected {
                    module: manifest.id,
                    detail: format!(
                        "profile {profile:?} is not implemented; this module implements {}",
                        manifest
                            .profiles_with_off()
                            .iter()
                            .map(|p| p.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
            for required in manifest.requires {
                let attached = self
                    .entries
                    .iter()
                    .find(|entry| entry.module.manifest().id == *required)
                    .map(|entry| entry.profile.is_attached())
                    .unwrap_or(false);
                if !attached {
                    return Err(ServiceError::ModuleRejected {
                        module: manifest.id,
                        detail: format!(
                            "requires module {required:?}, which is not attached; attach it first: {}",
                            attach_call(required, Profile::Real)
                        ),
                    });
                }
            }
        }
        if !profile.is_attached() {
            return self.detach(host, id);
        }
        if self.entries[index].profile.is_attached() {
            // Re-attach with a different profile: detach first so the module's
            // registrations are always covered by exactly one scope.
            self.detach(host, id)?;
        }
        // A tombstone for a name this module provides must go before the module
        // registers its own node: two nodes with the same descriptor would make
        // `checkService` answer the tombstone (which says "not attached").
        let replaced: Vec<&'static str> = {
            let manifest = self.entries[index].module.manifest();
            manifest
                .services
                .iter()
                .map(|service| service.name)
                .filter(|name| self.tombstoned.contains(name))
                .collect()
        };
        let scope = host.begin_scope();
        for name in &replaced {
            host.remove_service(name);
            self.tombstoned.retain(|tombstoned| tombstoned != name);
        }
        let attach_result = self.entries[index].module.attach(host, profile, config);
        match attach_result {
            Ok(()) => {
                let id = self.entries[index].module.manifest().id;
                self.entries[index].profile = profile;
                self.entries[index].scope = Some(scope);
                self.attached_order.push(id);
                Ok(())
            }
            Err(error) => {
                host.end_scope(scope);
                Err(error)
            }
        }
    }

    /// Detach `id`, removing every registration it made.
    pub fn detach(&mut self, host: &mut ServiceHost, id: &str) -> Result<()> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.module.manifest().id == id)
            .ok_or_else(|| ServiceError::ModuleRejected {
                module: "<launch>",
                detail: format!("unknown module {id:?}"),
            })?;
        if !self.entries[index].profile.is_attached() {
            // Detaching what is not attached is a no-op, not an error: a
            // scenario that asks for `off` twice must not fail the run.
            return Ok(());
        }
        self.entries[index].module.detach(host)?;
        if let Some(scope) = self.entries[index].scope.take() {
            host.end_scope(scope);
        }
        self.entries[index].profile = Profile::Off;
        let id = self.entries[index].module.manifest().id;
        self.attached_order.retain(|attached| *attached != id);
        // The name is now unserved again: put its tombstone back, so a detached
        // module's name never falls back to a silent null.
        let restore: Vec<&'static Tombstone> = self.entries[index]
            .module
            .manifest()
            .services
            .iter()
            .filter_map(|service| tombstone_for(service.name))
            .filter(|tombstone| tombstone.reaction == AospReaction::Tombstone)
            .filter(|tombstone| !self.tombstoned.contains(&tombstone.name))
            .collect();
        for tombstone in restore {
            install_tombstone(host, tombstone);
            self.tombstoned.push(tombstone.name);
        }
        Ok(())
    }

    /// Attach a module to a **live** session (`acos_services {op: "attach"}`).
    ///
    /// Only `hot` modules can come and go; an `at-launch` module refuses with a
    /// structured error naming the launch call, because its registrations are
    /// part of the service table the guest booted against (ADR-0029 item 4).
    pub fn attach_live(
        &mut self,
        host: &mut ServiceHost,
        id: &str,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        let manifest = self.manifest(id).ok_or_else(|| ServiceError::ModuleRejected {
            module: "<live>",
            detail: format!("unknown module {id:?}; known modules: {}", self.ids().join(", ")),
        })?;
        if manifest.attach != AttachMode::Hot {
            return Err(ServiceError::ModuleRejected {
                module: manifest.id,
                detail: format!(
                    "module is `at-launch` only and cannot be attached to a live session; attach it at launch: {}",
                    attach_call(manifest.id, default_profile(manifest))
                ),
            });
        }
        self.attach(host, id, profile, config)
    }

    /// Detach a module from a **live** session, with the same `hot` rule.
    pub fn detach_live(&mut self, host: &mut ServiceHost, id: &str) -> Result<()> {
        let manifest = self.manifest(id).ok_or_else(|| ServiceError::ModuleRejected {
            module: "<live>",
            detail: format!("unknown module {id:?}"),
        })?;
        if manifest.attach != AttachMode::Hot {
            return Err(ServiceError::ModuleRejected {
                module: manifest.id,
                detail: "module is `at-launch` only and cannot be detached from a live session"
                    .to_string(),
            });
        }
        self.detach(host, id)
    }

    /// Attach every requested spec, in order. `core` is attached first and
    /// always, at `real`.
    ///
    /// Used by the launch path (item 4) and by the tests: the same calls the
    /// agent API makes.
    pub fn attach_session(
        &mut self,
        host: &mut ServiceHost,
        specs: &[ModuleSpec],
    ) -> Result<Vec<(&'static str, Profile)>> {
        if self.contains("core") {
            self.attach(host, "core", Profile::Real, &ModuleConfig::default())?;
        }
        for spec in specs {
            self.attach(host, &spec.id, spec.profile, &spec.config)?;
        }
        Ok(self.attached())
    }

    /// The attached modules and their profiles, in attach order.
    #[must_use]
    pub fn attached(&self) -> Vec<(&'static str, Profile)> {
        self.entries
            .iter()
            .filter(|entry| entry.profile.is_attached())
            .map(|entry| (entry.module.manifest().id, entry.profile))
            .collect()
    }

    /// Every module's honest report, attached or not.
    #[must_use]
    pub fn reports(&self) -> Vec<ModuleReport> {
        self.entries
            .iter()
            .map(|entry| entry.module.report())
            .collect()
    }

    /// Drive scripted state on an attached module.
    pub fn control(&mut self, id: &str, op: &str, args: &Value) -> Result<Value> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.module.manifest().id == id)
            .ok_or_else(|| ServiceError::ModuleRejected {
                module: "<launch>",
                detail: format!("unknown module {id:?}"),
            })?;
        if !self.entries[index].profile.is_attached() {
            return Err(ServiceError::ModuleRejected {
                module: id_static(self.entries[index].module.as_ref()),
                detail: format!(
                    "module is off, so {op:?} was not applied (no fake success); attach it: {}",
                    attach_call(
                        id_static(self.entries[index].module.as_ref()),
                        default_profile(self.entries[index].module.manifest())
                    )
                ),
            });
        }
        self.entries[index].module.control(op, args)
    }

    /// Run `f` with the module object for `id` (tests and the launch reply).
    pub fn with_module<R>(&mut self, id: &str, f: impl FnOnce(&mut dyn Module) -> R) -> Option<R> {
        self.entries
            .iter_mut()
            .find(|entry| entry.module.manifest().id == id)
            .map(|entry| f(entry.module.as_mut()))
    }

    /// Register a tombstone for every gap-list name whose module is **not**
    /// attached, and return the names that now answer the structured error.
    ///
    /// Names kept `null` by the AOSP decision are not registered: they answer
    /// `null` like a phone without the feature, and appear as `off` in
    /// [`Self::status_json`] with the reason.
    pub fn install_tombstones(&mut self, host: &mut ServiceHost) -> Vec<&'static str> {
        let mut installed = Vec::new();
        for tombstone in TOMBSTONES {
            if tombstone.reaction != AospReaction::Tombstone {
                continue;
            }
            let attached = self
                .entries
                .iter()
                .any(|entry| {
                    entry.module.manifest().id == tombstone.module
                        && entry.profile.is_attached()
                });
            if attached {
                continue;
            }
            // A module that exists and is attached may legitimately register the
            // name itself; the registry only tombstones what nobody serves.
            if let Some(owner) = self.providers.get(tombstone.name) {
                let served = self.entries.iter().any(|entry| {
                    entry.module.manifest().id == *owner && entry.profile.is_attached()
                });
                if served {
                    continue;
                }
            }
            install_tombstone(host, tombstone);
            self.tombstoned.push(tombstone.name);
            installed.push(tombstone.name);
        }
        installed
    }

    /// The tombstone names currently registered.
    #[must_use]
    pub fn tombstoned(&self) -> &[&'static str] {
        &self.tombstoned
    }

    /// The generated capability payload: every module, every profile, what is
    /// attached, and every gap name with its AOSP decision.
    ///
    /// `acos_capabilities` and the `acos_help` limits section are both rendered
    /// from this (ADR-0029 item 6): one source, so the docs cannot drift.
    #[must_use]
    pub fn status_json(&self) -> Value {
        let modules: Vec<Value> = self
            .entries
            .iter()
            .map(|entry| {
                let manifest = entry.module.manifest();
                let mut value = manifest.to_json();
                if let Value::Object(map) = &mut value {
                    map.insert("profile".into(), json!(entry.profile.as_str()));
                    map.insert("attached".into(), json!(entry.profile.is_attached()));
                    map.insert(
                        "default_profile".into(),
                        json!(default_profile(manifest).as_str()),
                    );
                    map.insert(
                        "attach_call".into(),
                        json!(attach_call(manifest.id, default_profile(manifest))),
                    );
                }
                value
            })
            .collect();
        let gaps: Vec<Value> = TOMBSTONES
            .iter()
            .map(|tombstone| {
                let attached_owner = self
                    .entries
                    .iter()
                    .find(|entry| entry.module.manifest().id == tombstone.module)
                    .map(|entry| entry.profile.is_attached())
                    .unwrap_or(false);
                let state = if attached_owner {
                    "attached"
                } else {
                    match tombstone.reaction {
                        AospReaction::Tombstone => "tombstone",
                        AospReaction::Null => "off",
                    }
                };
                json!({
                    "service": tombstone.name,
                    "descriptor": tombstone.descriptor,
                    "module": tombstone.module,
                    "state": state,
                    "reason": tombstone.reason,
                    "aosp": if tombstone.reaction == AospReaction::Tombstone { "tombstone" } else { "null" },
                    "evidence": tombstone.evidence,
                })
            })
            .collect();
        json!({
            "attached": self.attached().iter().map(|(id, profile)| json!({
                "id": id,
                "profile": profile.as_str(),
            })).collect::<Vec<_>>(),
            "modules": modules,
            "gaps": gaps,
        })
    }

    /// The generated "service modules" section of the `acos_help` limits topic.
    ///
    /// Markdown, because it is spliced into the agent guide: a table of
    /// modules and a table of names this build does not publish, each with its
    /// state and the call that changes it.
    #[must_use]
    pub fn limits_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("### Service modules (generated from the module registry)\n\n");
        out.push_str(
            "A module is attached with `acos_launch {services: [{id, profile}]}` or the \
             `--service id[:profile]` CLI flag. `off` means the framework takes its real \
             \"missing\" path and every reply says so. `stub` and `scripted` answers are \
             never a real implementation and a verdict that relies on them says so.\n\n",
        );
        out.push_str("| module | profiles | attach | RAM | what it provides |\n");
        out.push_str("|---|---|---|---|---|\n");
        for manifest in self.manifests() {
            let provides: Vec<String> = manifest
                .services
                .iter()
                .map(|service| format!("`{}`", service.name))
                .chain(manifest.providers.iter().map(|p| format!("provider `{p}`")))
                .chain(manifest.windows.iter().map(|w| format!("window `{w}`")))
                .chain(
                    manifest
                        .system_apps
                        .iter()
                        .map(|app| format!("app `{app}`")),
                )
                .collect();
            out.push_str(&format!(
                "| `{}` | {} | {} | \u{2264}{} MiB | {} |\n",
                manifest.id,
                manifest
                    .profiles_with_off()
                    .iter()
                    .map(|profile| format!("`{profile}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                manifest.attach.as_str(),
                manifest.budget.ram_mib,
                provides.join(", ")
            ));
        }
        out.push('\n');
        out.push_str(
            "Names a phone publishes that this build does not (module `off`): each one \
             reports the module and the attach call, or answers Android's real missing \
             path where AOSP needs the null.\n\n",
        );
        out.push_str("| name | module | answer | why |\n|---|---|---|---|\n");
        for tombstone in TOMBSTONES {
            out.push_str(&format!(
                "| `{}` | `{}` | {} | {} |\n",
                tombstone.name,
                tombstone.module,
                if tombstone.reaction == AospReaction::Tombstone {
                    "structured `UNSUPPORTED_ANDROID_API`"
                } else {
                    "Android's missing path (`null`)"
                },
                tombstone.reason
            ));
        }
        out
    }
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn id_static(module: &dyn Module) -> &'static str {
    module.manifest().id
}

/// The generated capability payload for `acos_capabilities`.
///
/// One source (this registry), rendered by the MCP server: the module catalog
/// with every profile it implements, what the default session attaches, and
/// every gap name with its AOSP decision (ADR-0029 item 6).
#[must_use]
pub fn capabilities_json() -> Value {
    let registry = ModuleRegistry::standard();
    let mut status = registry.status_json();
    if let Value::Object(map) = &mut status {
        map.insert(
            "default_session".into(),
            json!([{"id": "core", "profile": "real"}]),
        );
        map.insert("core_attached".into(), json!(true));
    }
    status
}

/// Install the standard module set on `host`: attach `core` and tombstone every
/// gap name no module serves.
///
/// This is the one call a session makes before the guest starts. `core` is
/// always attached at `real`, so an existing session's service table is exactly
/// what it was before modules existed (ADR-0029 item 1) plus the tombstones.
pub fn install_standard(host: &mut ServiceHost) -> ModuleRegistry {
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core module attach cannot fail");
    registry.install_tombstones(host);
    registry
}

/// The conformance kit every module must pass (ADR-0029 item 7).
///
/// Six checks, all mechanical:
///
/// 1. **manifest** — [`ModuleManifest::validate`].
/// 2. **attach** — the module attaches at its default profile and every
///    `ServiceManager` name it provides resolves to a binder.
/// 3. **detach** — attach/detach twice returns registrations, service names,
///    threads and fds to the baseline (`/proc/self/fd` and
///    `/proc/self/task` are the process's own counters, so the test measures
///    the real cost, not a module's claim).
/// 4. **honest-report** — with the module detached, every name it provides is
///    either absent (the documented `null`) or a tombstone that answers a
///    structured `UNSUPPORTED_ANDROID_API` naming the module; none of them is
///    silently served, and `control` refuses instead of faking success.
/// 5. **negative** — an unknown profile and an unknown control op are refused.
/// 6. **budget** — the declared budget is non-zero and, for an in-process
///    module, within the framework ceiling the registry enforces.
pub mod conformance {
    use super::*;

    /// One check result.
    #[derive(Clone, Debug)]
    pub struct Check {
        /// Check name.
        pub name: &'static str,
        /// Whether it passed.
        pub pass: bool,
        /// What was measured or why it failed.
        pub detail: String,
    }

    /// The kit's report for one module.
    #[derive(Clone, Debug)]
    pub struct Report {
        /// Module id.
        pub module: &'static str,
        /// Every check, in order.
        pub checks: Vec<Check>,
    }

    impl Report {
        /// True when every check passed.
        #[must_use]
        pub fn pass(&self) -> bool {
            self.checks.iter().all(|check| check.pass)
        }

        /// The failing checks, rendered for a test assertion message.
        #[must_use]
        pub fn failures(&self) -> String {
            self.checks
                .iter()
                .filter(|check| !check.pass)
                .map(|check| format!("{}: {}", check.name, check.detail))
                .collect::<Vec<_>>()
                .join("; ")
        }
    }

    /// Process-wide baselines a detach must restore.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Baseline {
        /// Open file descriptors.
        pub fds: usize,
        /// Live threads.
        pub threads: usize,
        /// Registered binder handles.
        pub binders: usize,
        /// Registered `ServiceManager` names.
        pub names: usize,
    }

    /// Measure the process and the host.
    #[must_use]
    pub fn baseline(host: &ServiceHost) -> Baseline {
        Baseline {
            fds: count_entries("/proc/self/fd"),
            threads: count_entries("/proc/self/task"),
            binders: host.binder_registry().binders().len(),
            names: host.service_names().len(),
        }
    }

    fn count_entries(path: &str) -> usize {
        std::fs::read_dir(path)
            .map(|entries| entries.filter_map(std::result::Result::ok).count())
            .unwrap_or(0)
    }

    /// Run the kit for `module_id` on a fresh registry and host.
    ///
    /// The registry is [`ModuleRegistry::standard`] so the kit runs against the
    /// exact module the build ships.
    #[must_use]
    pub fn run(module_id: &str) -> Report {
        let mut registry = ModuleRegistry::standard();
        run_with(&mut registry, module_id)
    }

    /// Run the kit for `module_id` against an already-built registry.
    #[must_use]
    pub fn run_with(registry: &mut ModuleRegistry, module_id: &str) -> Report {
        let mut checks: Vec<Check> = Vec::new();
        let Some(manifest) = registry.manifest(module_id) else {
            return Report {
                module: "<unknown>",
                checks: vec![Check {
                    name: "manifest",
                    pass: false,
                    detail: format!("no module {module_id:?} in the registry"),
                }],
            };
        };

        // 1. manifest
        match manifest.validate() {
            Ok(()) => checks.push(Check {
                name: "manifest",
                pass: true,
                detail: format!(
                    "{} v{}: {} service(s), profiles {}",
                    manifest.id,
                    manifest.version,
                    manifest.services.len(),
                    manifest
                        .profiles_with_off()
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join("/")
                ),
            }),
            Err(error) => checks.push(Check {
                name: "manifest",
                pass: false,
                detail: error,
            }),
        }

        let default_profile = default_profile(manifest);
        let provides: Vec<&'static str> = manifest.services.iter().map(|s| s.name).collect();

        // 2. attach (twice, so check 3 has a second cycle). The tombstones a
        // session installs are part of the baseline: a detached module must come
        // back to "its name answers a tombstone", not to "its name is null".
        let mut host = ServiceHost::new();
        registry.install_tombstones(&mut host);
        let before = baseline(&host);
        let mut attach_ok = true;
        let mut attach_detail = String::new();
        for cycle in 0..2 {
            if let Err(error) =
                registry.attach(&mut host, module_id, default_profile, &ModuleConfig::default())
            {
                attach_ok = false;
                attach_detail = format!("attach #{cycle} failed: {error}");
                break;
            }
            for name in &provides {
                if host.check_service(name).ok().flatten().is_none() {
                    attach_ok = false;
                    attach_detail = format!(
                        "attach #{cycle}: module provides {name:?} but checkService answers null"
                    );
                }
            }
            let report = registry
                .reports()
                .into_iter()
                .find(|report| report.id == module_id)
                .expect("report for a known module");
            if !report.attached || report.profile != Some(default_profile) {
                attach_ok = false;
                attach_detail =
                    format!("attach #{cycle}: report says {report:?} after a successful attach");
            }
            if let Err(error) = registry.detach(&mut host, module_id) {
                attach_ok = false;
                attach_detail = format!("detach #{cycle} failed: {error}");
                break;
            }
        }
        checks.push(Check {
            name: "attach",
            pass: attach_ok,
            detail: if attach_ok {
                format!(
                    "attached at {default_profile} twice; {} name(s) resolved",
                    provides.len()
                )
            } else {
                attach_detail
            },
        });

        // 3. detach restores the process and the host
        let after = baseline(&host);
        let restored = after == before;
        checks.push(Check {
            name: "detach-restores",
            pass: restored,
            detail: format!("baseline {before:?} after {after:?}"),
        });

        // 4. honest report: nothing is served, nothing fakes success
        let mut honest = true;
        let mut honest_detail = String::new();
        for name in &provides {
            match host.check_service(name).ok().flatten() {
                None => {}
                Some(binder) => match transact_once(&mut host, binder) {
                    Err(ServiceError::ModuleUnavailable { module, .. }) if module == module_id => {}
                    other => {
                        honest = false;
                        honest_detail = format!(
                            "detached module still answers {name:?}: {other:?} (expected null or a tombstone naming {module_id})"
                        );
                    }
                },
            }
        }
        if honest {
            match registry.control(module_id, "no-such-op", &Value::Null) {
                Err(ServiceError::ModuleRejected { .. }) => {}
                other => {
                    honest = false;
                    honest_detail = format!(
                        "control on a detached module answered {other:?} instead of a refusal"
                    );
                }
            }
        }
        if honest {
            let report = registry
                .reports()
                .into_iter()
                .find(|report| report.id == module_id)
                .expect("report for a known module");
            if report.attached || report.profile.is_some() || report.reason.is_none() {
                honest = false;
                honest_detail = format!(
                    "detached module's report is not honest: attached={} profile={:?} reason={:?}",
                    report.attached, report.profile, report.reason
                );
            }
        }
        checks.push(Check {
            name: "honest-report",
            pass: honest,
            detail: if honest {
                format!(
                    "{} name(s) answer null or a tombstone; control refuses; report says off",
                    provides.len()
                )
            } else {
                honest_detail
            },
        });

        // 5. negative: a profile the module does not implement and an unknown op
        let mut negative = true;
        let mut negative_detail = String::new();
        let unimplemented = Profile::ALL
            .into_iter()
            .find(|profile| !manifest.supports(*profile));
        if let Some(profile) = unimplemented {
            match registry.attach(&mut host, module_id, profile, &ModuleConfig::default()) {
                Err(ServiceError::ModuleRejected { .. }) => {}
                other => {
                    negative = false;
                    negative_detail =
                        format!("attaching unimplemented profile {profile} answered {other:?}");
                }
            }
        }
        if negative {
            if registry
                .attach(&mut host, module_id, default_profile, &ModuleConfig::default())
                .is_ok()
            {
                match registry.control(module_id, "no-such-op", &Value::Null) {
                    Err(ServiceError::ModuleRejected { .. }) => {}
                    other => {
                        negative = false;
                        negative_detail =
                            format!("unknown control op answered {other:?} instead of a refusal");
                    }
                }
                let _ = registry.detach(&mut host, module_id);
            }
        }
        if negative {
            match registry.attach(
                &mut host,
                "definitely.not.a.module",
                Profile::Stub,
                &ModuleConfig::default(),
            ) {
                Err(ServiceError::ModuleRejected { .. }) => {}
                other => {
                    negative = false;
                    negative_detail = format!("unknown module id answered {other:?}");
                }
            }
        }
        checks.push(Check {
            name: "negative",
            pass: negative,
            detail: if negative {
                "unimplemented profile, unknown control op and unknown module id all refused"
                    .to_string()
            } else {
                negative_detail
            },
        });

        // 6. budget
        let budget_ok = manifest.budget.ram_mib > 0 && manifest.budget.startup_ms > 0;
        checks.push(Check {
            name: "budget",
            pass: budget_ok,
            detail: format!(
                "{} MiB / {} ms declared",
                manifest.budget.ram_mib, manifest.budget.startup_ms
            ),
        });

        Report {
            module: manifest.id,
            checks,
        }
    }

    /// One transaction on `binder` with the descriptor's interface token, so a
    /// tombstone is exercised exactly as a framework proxy would exercise it.
    fn transact_once(
        host: &mut ServiceHost,
        binder: acos_parcel::BinderId,
    ) -> Result<Vec<crate::value::Value>> {
        let descriptor = host
            .descriptor_of(binder)
            .unwrap_or("<unknown>")
            .to_string();
        let mut data = acos_parcel::Parcel::new();
        data.write_interface_token(&descriptor)?;
        data.set_data_position(0);
        let mut reply = acos_parcel::Parcel::new();
        host.transact(binder, 1, &mut data, &mut reply, 0)?;
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_round_trip() {
        for profile in Profile::ALL {
            assert_eq!(Profile::parse(profile.as_str()), Some(profile));
        }
        assert_eq!(Profile::parse("REAL"), None);
        assert!(!Profile::Off.is_attached());
        assert!(Profile::Stub.is_attached());
    }

    #[test]
    fn a_spec_parses_the_cli_spelling_and_refuses_a_typo() {
        let spec = ModuleSpec::parse("feedback.vibrator:stub").expect("valid spec");
        assert_eq!(spec.id, "feedback.vibrator");
        assert_eq!(spec.profile, Profile::Stub);
        assert_eq!(ModuleSpec::parse("core").unwrap().profile, Profile::Real);
        let error = ModuleSpec::parse("core:rela").expect_err("typo must fail");
        assert!(matches!(error, ServiceError::ModuleRejected { .. }));
        assert!(ModuleSpec::parse(":stub").is_err());
    }

    #[test]
    fn the_standard_registry_manifests_are_valid_and_unique() {
        let registry = ModuleRegistry::standard();
        assert!(registry.contains("core"));
        for manifest in registry.manifests() {
            manifest.validate().expect("standard manifests are valid");
        }
        // Two modules cannot provide the same name.
        let mut registry = ModuleRegistry::standard();
        let duplicate = Box::new(crate::modules::vibrator::VibratorModule::new());
        let error = registry.add(duplicate).expect_err("duplicate provides");
        assert!(error.contains("vibrator_manager"), "{error}");
    }

    #[test]
    fn every_gap_name_has_a_decision_and_a_reason() {
        for tombstone in TOMBSTONES {
            assert!(!tombstone.reason.is_empty(), "{}", tombstone.name);
            assert!(!tombstone.evidence.is_empty(), "{}", tombstone.name);
            assert!(!tombstone.module.is_empty(), "{}", tombstone.name);
            assert!(!tombstone.descriptor.is_empty(), "{}", tombstone.name);
        }
    }
}
