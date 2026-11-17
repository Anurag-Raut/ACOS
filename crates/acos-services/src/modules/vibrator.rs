//! `feedback.vibrator` — the reference module (stub, scripted).
//!
//! This is the small module the conformance kit is demonstrated on: it owns one
//! `ServiceManager` name (`vibrator_manager`), serves
//! `android.os.IVibratorManagerService` with fixed answers, and records what the
//! app asked for so a `scripted` run can assert it.
//!
//! It has **no `real` profile**, on purpose. ACOS has no vibration hardware, and
//! a module that claimed `real` while doing nothing would be exactly the fake
//! success ADR-0029 forbids. What `stub` answers, from the AIDL method order in
//! `android/os/IVibratorManagerService.aidl`:
//!
//! | code | method | answer |
//! |---|---|---|
//! | 1 | `getVibratorIds()` | `[0]` — one vibrator, so `Vibrator.hasVibrator()` is true |
//! | 2 | `getVibratorInfo(int)` | structured `UNSUPPORTED_ANDROID_API` (a `VibratorInfo` parcelable would be a fabrication) |
//! | 3 | `isVibrating(int)` | `false` (recorded) |
//! | 4 | `registerVibratorStateListener` | `false` — no state events are delivered |
//! | 5 | `unregisterVibratorStateListener` | `false` |
//! | 6 | `setAlwaysOnEffect(...)` | `false` — not supported |
//! | 7 | `vibrate(...)` | recorded, no-op (void) |
//! | 8 | `cancelVibrate(...)` | recorded, no-op (void) |
//! | 9 | `performHapticFeedback(...)` | recorded, no-op (oneway) |
//!
//! `control` ops: `calls` (the recorded counts), `reset` (clear them).

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::exception::write_no_exception;
use crate::host::{CallResult, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};

/// The `ServiceManager` name this module owns.
pub const SERVICE_NAME: &str = "vibrator_manager";
/// The AIDL descriptor of the interface it serves.
pub const DESCRIPTOR: &str = "android.os.IVibratorManagerService";
/// The module id.
pub const MODULE_ID: &str = "feedback.vibrator";

/// One `vibrate(...)` call, as the scripted profile records it.
///
/// The `CombinedVibration` payload is recorded by length and FNV-1a hash of the
/// request parcel, plus the `CombinedVibration` token (`1` mono, `2` stereo,
/// `3` sequential, `0` unreadable). Decoding the whole nested
/// `VibrationEffect`/`VibrationAttributes` parcel graph would mean
/// re-implementing `android.os.vibrator`'s parcelables; a hash is enough to
/// assert "a pattern arrived" and "these two patterns differ", and the report
/// says that is what it is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VibratePattern {
    /// `uid` argument.
    pub uid: i32,
    /// `deviceId` argument.
    pub device_id: i32,
    /// `opPkg` argument.
    pub op_pkg: String,
    /// `CombinedVibration` token (`1`/`2`/`3`), `0` when absent/unreadable.
    pub token: i32,
    /// Bytes after `opPkg` (the `CombinedVibration` typed object onward).
    pub payload_len: usize,
    /// FNV-1a hash of the whole request parcel (pattern identity).
    pub hash: String,
}

/// What the app asked the vibrator to do.
#[derive(Debug, Default)]
pub struct VibratorState {
    /// `vibrate(...)` calls (a pattern is not decoded: `CombinedVibration` is a
    /// nested parcelable and a stub has nothing to do with its contents).
    pub vibrate_calls: u32,
    /// `cancelVibrate(...)` calls.
    pub cancel_calls: u32,
    /// `performHapticFeedback(...)` calls.
    pub haptic_calls: u32,
    /// `isVibrating(...)` / `getVibratorIds()` calls.
    pub query_calls: u32,
    /// Every `vibrate(...)` call's recorded pattern (bounded to the last 64).
    pub patterns: Vec<VibratePattern>,
    /// Calls that were answered with a structured unsupported error.
    pub unsupported: Vec<String>,
}

impl VibratorState {
    /// The recorded state as JSON (the `calls` control op and `report`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "vibrate_calls": self.vibrate_calls,
            "cancel_calls": self.cancel_calls,
            "haptic_calls": self.haptic_calls,
            "query_calls": self.query_calls,
            "patterns": self.patterns.iter().map(|pattern| json!({
                "uid": pattern.uid,
                "device_id": pattern.device_id,
                "op_pkg": pattern.op_pkg,
                "token": pattern.token,
                "payload_len": pattern.payload_len,
                "hash": pattern.hash,
            })).collect::<Vec<_>>(),
            "unsupported": self.unsupported,
        })
    }
}

const CONTROL_OPS: [ControlOp; 3] = [
    ControlOp {
        name: "calls",
        args: "none — returns the recorded vibrate/cancel/haptic/query counts and every recorded pattern (token, length, hash)",
    },
    ControlOp {
        name: "patterns",
        args: "none — the recorded `vibrate(...)` patterns only",
    },
    ControlOp {
        name: "reset",
        args: "none — clears the recorded counts",
    },
];

/// The manifest. Static, so `Module::manifest` can hand out `&'static`.
fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "vibrator manager: stub/scripted (no vibration hardware), records what the app asked for",
        services: &[ServiceProvide {
            name: SERVICE_NAME,
            descriptor: DESCRIPTOR,
        }],
        replaces: &[],
        process_initializers: &[],
        features: &[],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &[],
        profiles: &[Profile::Stub, Profile::Scripted],
        budget: Budget {
            ram_mib: 1,
            startup_ms: 5,
        },
        attach: AttachMode::Hot,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

/// The module.
pub struct VibratorModule {
    profile: Profile,
    state: Option<Arc<Mutex<VibratorState>>>,
}

impl VibratorModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            state: None,
        }
    }

    /// The manifest (used by the tests and the registry).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }
}

/// FNV-1a over the request parcel: the pattern's identity for assertions.
fn fnv1a(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

impl Default for VibratorModule {
    fn default() -> Self {
        Self::new()
    }
}

struct VibratorService {
    state: Arc<Mutex<VibratorState>>,
}

impl VibratorService {
    fn state(&self) -> std::sync::MutexGuard<'_, VibratorState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn unsupported(&self, method: &str) -> ServiceError {
        {
            let mut state = self.state();
            state.unsupported.push(method.to_string());
        }
        ServiceError::UnsupportedAndroidApi {
            interface: DESCRIPTOR.to_string(),
            code: 0,
            method: format!("{MODULE_ID}: {method} has no stub answer"),
        }
    }
}

impl ServiceObject for VibratorService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(self.unsupported(&format!("{interface}::{}", method.name)))
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != DESCRIPTOR {
            return Ok(false);
        }
        match code {
            // int[] getVibratorIds()
            1 => {
                self.state().query_calls += 1;
                write_no_exception(reply);
                reply.write_int32(1);
                reply.write_int32(0);
                Ok(true)
            }
            // VibratorInfo getVibratorInfo(int) — a parcelable this module would
            // have to invent.
            2 => Err(self.unsupported("getVibratorInfo")),
            // boolean isVibrating(int)
            3 => {
                self.state().query_calls += 1;
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            // boolean registerVibratorStateListener / unregisterVibratorStateListener
            4 | 5 => {
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            // boolean setAlwaysOnEffect(...)
            6 => {
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            // void vibrate(int uid, int deviceId, String opPkg,
            //              CombinedVibration vibration, VibrationAttributes
            //              attributes, String reason, IBinder token)
            7 => {
                let uid = data
                    .read_int32()
                    .map_err(|error| ServiceError::BadRequest {
                        interface: DESCRIPTOR.to_string(),
                        method: "vibrate".to_string(),
                        detail: error.to_string(),
                    })?;
                let device_id = data
                    .read_int32()
                    .map_err(|error| ServiceError::BadRequest {
                        interface: DESCRIPTOR.to_string(),
                        method: "vibrate".to_string(),
                        detail: error.to_string(),
                    })?;
                let op_pkg = data
                    .read_string16()
                    .map_err(|error| ServiceError::BadRequest {
                        interface: DESCRIPTOR.to_string(),
                        method: "vibrate".to_string(),
                        detail: error.to_string(),
                    })?
                    .unwrap_or_default();
                let payload_start = data.data_position();
                // AIDL writes the `CombinedVibration` as a typed object: a
                // presence word, then the parcelable body whose first word is the
                // type token.
                let presence = data.read_int32().unwrap_or(0);
                let token = if presence != 0 {
                    data.read_int32().unwrap_or(0)
                } else {
                    0
                };
                let payload_len = data.data_size().saturating_sub(payload_start);
                let hash = fnv1a(&data.marshall());
                {
                    let mut state = self.state();
                    state.vibrate_calls += 1;
                    if state.patterns.len() >= 64 {
                        state.patterns.remove(0);
                    }
                    state.patterns.push(VibratePattern {
                        uid,
                        device_id,
                        op_pkg,
                        token,
                        payload_len,
                        hash,
                    });
                }
                write_no_exception(reply);
                Ok(true)
            }
            // void cancelVibrate(int, IBinder)
            8 => {
                self.state().cancel_calls += 1;
                write_no_exception(reply);
                Ok(true)
            }
            // oneway void performHapticFeedback(...)
            9 => {
                self.state().haptic_calls += 1;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

impl Module for VibratorModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        _config: &ModuleConfig,
    ) -> Result<()> {
        if !matches!(profile, Profile::Stub | Profile::Scripted) {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "{profile} is not implemented: there is no vibration hardware; use `stub` or `scripted`"
                ),
            });
        }
        let state = Arc::new(Mutex::new(VibratorState::default()));
        host.register_dynamic_named(
            SERVICE_NAME,
            DESCRIPTOR,
            Box::new(VibratorService {
                state: Arc::clone(&state),
            }),
        );
        self.state = Some(state);
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        // The registration is rolled back by the registry's scope; dropping the
        // state here is what makes a second attach start clean.
        self.state = None;
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, _host: &mut ServiceHost, op: &str, _args: &Value) -> Result<Value> {
        let Some(state) = self.state.as_ref() else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("module is off, so {op:?} was not applied"),
            });
        };
        match op {
            "calls" => Ok(state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .to_json()),
            "patterns" => Ok(json!({
                "patterns": state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .patterns
                    .iter()
                    .map(|pattern| json!({
                        "uid": pattern.uid,
                        "device_id": pattern.device_id,
                        "op_pkg": pattern.op_pkg,
                        "token": pattern.token,
                        "payload_len": pattern.payload_len,
                        "hash": pattern.hash,
                    }))
                    .collect::<Vec<_>>(),
            })),
            "reset" => {
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *state = VibratorState::default();
                Ok(json!({"reset": true}))
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
        let manifest = manifest_ref();
        let mut report = ModuleReport::of(manifest, self.profile);
        if self.profile.is_attached() {
            // Attached, but not real: say so in the report itself, so a verdict
            // that relied on it cannot pretend it was hardware.
            let (vibrate_calls, patterns) = self
                .state
                .as_ref()
                .map(|state| {
                    let state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    (state.vibrate_calls, state.patterns.len())
                })
                .unwrap_or((0, 0));
            report.reason = Some(format!(
                "{} profile: fixed answers, no vibration hardware; {vibrate_calls} vibrate call(s), {patterns} pattern(s) recorded as token/length/hash of the CombinedVibration parcel (the nested VibrationEffect parcelables are not decoded)",
                self.profile
            ));
        }
        report
    }
}
