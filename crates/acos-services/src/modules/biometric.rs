//! `hw.biometric` — biometrics with a scripted result (stub, scripted).
//!
//! The framework's client for biometrics is `IAuthService` (`Context.AUTH_SERVICE`
//! = `auth`): `BiometricManager` transacts `canAuthenticate` and `BiometricPrompt`
//! transacts `authenticate` with an `IBiometricServiceReceiver` callback. The
//! module provides `auth` with the image's transaction codes and delivers the
//! scripted outcome to that receiver, so the app's
//! `AuthenticationCallback` really runs.
//!
//! | scripted result | callback |
//! |---|---|
//! | `success` | `onAuthenticationSucceeded(TYPE_FINGERPRINT)` (code 1) |
//! | `failed` | `onAuthenticationFailed()` (code 2) |
//! | `error` | `onError(modality, error, vendorCode)` (code 3) |
//! | `cancel` | `onError(TYPE_FINGERPRINT, BIOMETRIC_ERROR_USER_CANCELED, 0)` |
//! | `lockout` | `onError(TYPE_FINGERPRINT, BIOMETRIC_ERROR_LOCKOUT, 0)` |
//!
//! ## What is not there, and is reported as a gap
//!
//! * **No prompt UI.** A real `BiometricPrompt.authenticate` shows the system
//!   biometric dialog (the window layer stack's job). ACOS shows nothing: the
//!   callback fires without a visible prompt, and every report says so. A
//!   verdict that relies on this module must not claim a prompt was displayed.
//! * **No hardware, no enrolment store.** `canAuthenticate` answers whatever the
//!   script says; `fingerprint` (the old HAL binder) stays the tombstone it is,
//!   because there is no fingerprint HAL either.
//! * **No crypto-bound auth.** `CryptoObject` operation ids are recorded and
//!   never validated; a real key-unlocking result is not faked.

use std::sync::{Arc, Mutex};

use acos_parcel::BinderId;
use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::exception::write_no_exception;
use crate::host::{CallResult, PendingDelivery, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};

/// The module id (`TOMBSTONES` already names it for `auth`/`fingerprint`).
pub const MODULE_ID: &str = "hw.biometric";
/// `IAuthService` descriptor.
pub const AUTH_DESCRIPTOR: &str = "android.hardware.biometrics.IAuthService";
/// `IBiometricServiceReceiver` descriptor (the client's callback).
pub const RECEIVER_DESCRIPTOR: &str = "android.hardware.biometrics.IBiometricServiceReceiver";

/// `BiometricManager.BIOMETRIC_SUCCESS`.
pub const BIOMETRIC_SUCCESS: i32 = 0;
/// `BiometricManager.BIOMETRIC_ERROR_HW_UNAVAILABLE`.
pub const BIOMETRIC_ERROR_HW_UNAVAILABLE: i32 = 1;
/// `BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED`.
pub const BIOMETRIC_ERROR_NONE_ENROLLED: i32 = 11;
/// `BiometricManager.BIOMETRIC_ERROR_NO_HARDWARE`.
pub const BIOMETRIC_ERROR_NO_HARDWARE: i32 = 12;
/// `BiometricManager.BIOMETRIC_ERROR_SECURITY_UPDATE_REQUIRED`.
pub const BIOMETRIC_ERROR_SECURITY_UPDATE_REQUIRED: i32 = 15;

/// `BiometricAuthenticator.TYPE_FINGERPRINT`.
pub const TYPE_FINGERPRINT: i32 = 2;
/// `BiometricConstants.BIOMETRIC_ERROR_CANCELED`.
pub const BIOMETRIC_ERROR_CANCELED: i32 = 5;
/// `BiometricConstants.BIOMETRIC_ERROR_LOCKOUT`.
pub const BIOMETRIC_ERROR_LOCKOUT: i32 = 7;
/// `BiometricConstants.BIOMETRIC_ERROR_USER_CANCELED`.
pub const BIOMETRIC_ERROR_USER_CANCELED: i32 = 10;

/// `IAuthService.authenticate(...)`.
const AUTH_AUTHENTICATE: u32 = 4;
/// `IAuthService.cancelAuthentication(...)`.
const AUTH_CANCEL_AUTHENTICATION: u32 = 5;
/// `IAuthService.canAuthenticate(String, int, int)`.
const AUTH_CAN_AUTHENTICATE: u32 = 6;
/// `IAuthService.hasEnrolledBiometrics(int, String)`.
const AUTH_HAS_ENROLLED_BIOMETRICS: u32 = 7;
/// `IAuthService.getAuthenticatorIds(int)`.
const AUTH_GET_AUTHENTICATOR_IDS: u32 = 10;

/// `IBiometricServiceReceiver.onAuthenticationSucceeded(int)`.
const RECEIVER_SUCCEEDED: u32 = 1;
/// `IBiometricServiceReceiver.onAuthenticationFailed()`.
const RECEIVER_FAILED: u32 = 2;
/// `IBiometricServiceReceiver.onError(int, int, int)`.
const RECEIVER_ERROR: u32 = 3;

/// One pending `BiometricPrompt.authenticate` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAuth {
    /// The `IBiometricServiceReceiver` binder.
    pub receiver: u64,
    /// The `CryptoObject` operation id (`0` = none).
    pub operation_id: i64,
    /// Calling package.
    pub package: String,
    /// The request id this module returned.
    pub auth_id: i64,
}

/// The session's biometric state.
#[derive(Debug)]
pub struct BiometricState {
    /// `canAuthenticate` answer.
    pub can_authenticate: i32,
    /// `hasEnrolledBiometrics` answer.
    pub enrolled: bool,
    /// Authentications the app started and that are still pending.
    pub pending: Vec<PendingAuth>,
    /// Authentications answered, with the scripted result.
    pub answered: Vec<String>,
    /// `cancelAuthentication` calls.
    pub cancels: u32,
    /// Calls answered with the structured unsupported error.
    pub unsupported: Vec<String>,
    /// The strong references the host holds on the callbacks this module
    /// stored: one per pending `authenticate` (`IBiometricServiceReceiver`).
    /// On Android storing the receiver in the service *is* `sp<IBinder>`;
    /// here it keeps the guest's node rooted for exactly as long as the
    /// request is pending (`crate::refs`).
    pub refs: crate::refs::GuestRefs,
}

impl Default for BiometricState {
    fn default() -> Self {
        Self {
            can_authenticate: BIOMETRIC_SUCCESS,
            enrolled: true,
            pending: Vec::new(),
            answered: Vec::new(),
            cancels: 0,
            unsupported: Vec::new(),
            refs: crate::refs::GuestRefs::new(),
        }
    }
}

impl BiometricState {
    /// The state as JSON (the `state` control op and `report`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "can_authenticate": self.can_authenticate,
            "enrolled": self.enrolled,
            "pending": self.pending.iter().map(|auth| json!({
                "operation_id": auth.operation_id,
                "package": auth.package,
                "auth_id": auth.auth_id,
            })).collect::<Vec<_>>(),
            "answered": self.answered,
            "cancels": self.cancels,
            "unsupported": self.unsupported,
        })
    }
}

const CONTROL_OPS: [ControlOp; 4] = [
    ControlOp {
        name: "state",
        args: "none — canAuthenticate answer, enrolled flag, pending and answered authentications",
    },
    ControlOp {
        name: "set",
        args: "{can_authenticate?, enrolled?} — can_authenticate is success|no_hardware|none_enrolled|hw_unavailable|security_update",
    },
    ControlOp {
        name: "auth",
        args: "{result: success|failed|error|cancel|lockout, modality?, error?, vendor_code?} — answer the pending authenticate call; `error` needs modality/error (constants or names)",
    },
    ControlOp {
        name: "reset",
        args: "none — clear pending/answered records (not the scripted answers)",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "biometrics: scripted canAuthenticate and BiometricPrompt authenticate results (no hardware, no prompt UI)",
        process_initializers: &[],
        features: &[],
        services: &[ServiceProvide {
            name: "auth",
            descriptor: AUTH_DESCRIPTOR,
        }],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &[],
        // Nothing is taken over from `core` (MODNOTIF5 unified the field).
        replaces: &[],
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
pub struct BiometricModule {
    profile: Profile,
    state: Option<Arc<Mutex<BiometricState>>>,
}

impl BiometricModule {
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

impl Default for BiometricModule {
    fn default() -> Self {
        Self::new()
    }
}

fn rejected(detail: impl Into<String>) -> ServiceError {
    ServiceError::ModuleRejected {
        module: MODULE_ID,
        detail: detail.into(),
    }
}

fn state_of(state: &Arc<Mutex<BiometricState>>) -> std::sync::MutexGuard<'_, BiometricState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct AuthService {
    state: Arc<Mutex<BiometricState>>,
    queued: Vec<PendingDelivery>,
    next_auth_id: i64,
}

impl ServiceObject for AuthService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: format!("{MODULE_ID}: {} has no stub answer", method.name),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != AUTH_DESCRIPTOR {
            return Ok(false);
        }
        let mut state = state_of(&self.state);
        match code {
            AUTH_CAN_AUTHENTICATE => {
                let _pkg = data.read_string16().map_err(ServiceError::Parcel)?;
                let _user = data.read_int32().map_err(ServiceError::Parcel)?;
                let _authenticators = data.read_int32().map_err(ServiceError::Parcel)?;
                write_no_exception(reply);
                reply.write_int32(state.can_authenticate);
                Ok(true)
            }
            AUTH_HAS_ENROLLED_BIOMETRICS => {
                let _user = data.read_int32().map_err(ServiceError::Parcel)?;
                let _pkg = data.read_string16().map_err(ServiceError::Parcel)?;
                let enrolled = state.enrolled;
                write_no_exception(reply);
                reply.write_int32(i32::from(enrolled));
                Ok(true)
            }
            AUTH_GET_AUTHENTICATOR_IDS => {
                // An empty `long[]`: no authenticator has ever been enrolled, so
                // no authenticator id exists. `BiometricManager` reads this as
                // "no ids", which is honest for a scripted device.
                let _user = data.read_int32().map_err(ServiceError::Parcel)?;
                write_no_exception(reply);
                reply
                    .write_int64_array(Some(&[]))
                    .map_err(ServiceError::Parcel)?;
                Ok(true)
            }
            AUTH_AUTHENTICATE => {
                // `long authenticate(IBinder token, long operationId, int userId,
                // IBiometricServiceReceiver receiver, String opPackageName,
                // in PromptInfo promptInfo)`.
                let _token = data.read_strong_binder().map_err(ServiceError::Parcel)?;
                let operation_id = data.read_int64().map_err(ServiceError::Parcel)?;
                let _user = data.read_int32().map_err(ServiceError::Parcel)?;
                let receiver = data
                    .read_strong_binder()
                    .map_err(ServiceError::Parcel)?
                    .map(|id| id.0)
                    .ok_or_else(|| ServiceError::BadRequest {
                        interface: AUTH_DESCRIPTOR.to_string(),
                        method: "authenticate".to_string(),
                        detail: "no IBiometricServiceReceiver binder in the request".into(),
                    })?;
                let package = data
                    .read_string16()
                    .map_err(ServiceError::Parcel)?
                    .unwrap_or_default();
                let auth_id = self.next_auth_id;
                self.next_auth_id += 1;
                // The service now holds the receiver: a strong reference on the
                // guest's node for as long as the request is pending.
                state.refs.hold(BinderId(receiver));
                state.pending.push(PendingAuth {
                    receiver,
                    operation_id,
                    package,
                    auth_id,
                });
                write_no_exception(reply);
                reply.write_int64(auth_id);
                Ok(true)
            }
            AUTH_CANCEL_AUTHENTICATION => {
                // `cancelAuthentication(IBinder token, String opPackageName,
                // long requestId)`: the app cancelled (or `CancellationSignal`
                // fired). A real service would answer the receiver with
                // `BIOMETRIC_ERROR_CANCELED`; do the same.
                let _token = data.read_strong_binder().map_err(ServiceError::Parcel)?;
                let _pkg = data.read_string16().map_err(ServiceError::Parcel)?;
                let request_id = data.read_int64().map_err(ServiceError::Parcel)?;
                state.cancels += 1;
                let delivered = state
                    .pending
                    .iter()
                    .position(|auth| auth.auth_id == request_id)
                    .map(|index| {
                        let auth = state.pending.remove(index);
                        auth.receiver
                    });
                if let Some(receiver) = delivered {
                    // The request is answered and gone: the reference goes with
                    // it (one shot, like `location`'s one-shot forms).
                    state.refs.release(BinderId(receiver));
                    self.queued.push(error_delivery(
                        receiver,
                        TYPE_FINGERPRINT,
                        BIOMETRIC_ERROR_CANCELED,
                        0,
                    ));
                    state.answered.push("canceled-by-client".to_string());
                }
                write_no_exception(reply);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn drain_raw_deliveries(&mut self) -> Vec<PendingDelivery> {
        std::mem::take(&mut self.queued)
    }

    fn drain_retained(&mut self) -> Vec<BinderId> {
        state_of(&self.state).refs.drain_retained()
    }

    fn drain_released(&mut self) -> Vec<BinderId> {
        state_of(&self.state).refs.drain_released()
    }

    fn release_all_refs(&mut self) -> Vec<BinderId> {
        state_of(&self.state).refs.release_all()
    }
}

fn receiver_delivery(target: u64, code: u32, method: &str, args: &[i32]) -> PendingDelivery {
    let mut data = acos_parcel::Parcel::new();
    data.write_interface_token(RECEIVER_DESCRIPTOR).ok();
    for value in args {
        data.write_int32(*value);
    }
    PendingDelivery {
        target: acos_parcel::BinderId(target),
        interface: RECEIVER_DESCRIPTOR.to_string(),
        code,
        method: method.to_string(),
        data,
    }
}

fn error_delivery(target: u64, modality: i32, error: i32, vendor_code: i32) -> PendingDelivery {
    receiver_delivery(
        target,
        RECEIVER_ERROR,
        "onError",
        &[modality, error, vendor_code],
    )
}

fn parse_can_authenticate(text: &str) -> Result<i32> {
    Ok(match text {
        "success" => BIOMETRIC_SUCCESS,
        "no_hardware" => BIOMETRIC_ERROR_NO_HARDWARE,
        "none_enrolled" => BIOMETRIC_ERROR_NONE_ENROLLED,
        "hw_unavailable" => BIOMETRIC_ERROR_HW_UNAVAILABLE,
        "security_update" => BIOMETRIC_ERROR_SECURITY_UPDATE_REQUIRED,
        other => {
            return Err(rejected(format!(
                "unknown can_authenticate {other:?}; use success, no_hardware, none_enrolled, hw_unavailable or security_update"
            )))
        }
    })
}

fn parse_modality(value: Option<&Value>) -> Result<i32> {
    match value {
        None => Ok(TYPE_FINGERPRINT),
        Some(Value::Number(number)) => number
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| rejected("modality must be an integer")),
        Some(Value::String(text)) => Ok(match text.as_str() {
            "fingerprint" => TYPE_FINGERPRINT,
            "face" => 8,
            "iris" => 4,
            other => {
                return Err(rejected(format!(
                    "unknown modality {other:?}; use fingerprint, face, iris or an int"
                )))
            }
        }),
        Some(other) => Err(rejected(format!(
            "modality must be a string or int, got {other}"
        ))),
    }
}

fn parse_biometric_error(value: Option<&Value>) -> Result<i32> {
    match value {
        None => Err(rejected(
            "an `error` result needs an `error` (a BIOMETRIC_ERROR_* name or int)",
        )),
        Some(Value::Number(number)) => number
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| rejected("error must be an integer")),
        Some(Value::String(text)) => Ok(match text.as_str() {
            "hw_unavailable" => BIOMETRIC_ERROR_HW_UNAVAILABLE,
            "canceled" => BIOMETRIC_ERROR_CANCELED,
            "lockout" => BIOMETRIC_ERROR_LOCKOUT,
            "lockout_permanent" => 9,
            "user_canceled" => BIOMETRIC_ERROR_USER_CANCELED,
            "no_hardware" => BIOMETRIC_ERROR_NO_HARDWARE,
            "none_enrolled" => BIOMETRIC_ERROR_NONE_ENROLLED,
            other => {
                return Err(rejected(format!(
                    "unknown biometric error {other:?}; use hw_unavailable, canceled, lockout, lockout_permanent, user_canceled, no_hardware, none_enrolled or an int"
                )))
            }
        }),
        Some(other) => Err(rejected(format!("error must be a string or int, got {other}"))),
    }
}

impl Module for BiometricModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        if !matches!(profile, Profile::Stub | Profile::Scripted) {
            return Err(rejected(format!(
                "{profile} is not implemented: ACOS has no biometric hardware, so use `stub` or `scripted`"
            )));
        }
        let mut state = BiometricState::default();
        for (key, value) in config.to_json().as_object().cloned().unwrap_or_default() {
            match key.as_str() {
                "can_authenticate" => {
                    let Some(text) = value.as_str() else {
                        return Err(rejected("can_authenticate must be a string"));
                    };
                    state.can_authenticate = parse_can_authenticate(text)?;
                }
                "enrolled" => {
                    let Some(flag) = value.as_bool() else {
                        return Err(rejected("enrolled must be a boolean"));
                    };
                    state.enrolled = flag;
                }
                other => {
                    return Err(rejected(format!(
                        "unknown config field {other:?}; this module accepts can_authenticate and enrolled"
                    )))
                }
            }
        }
        let state = Arc::new(Mutex::new(state));
        host.register_dynamic_named(
            "auth",
            AUTH_DESCRIPTOR,
            Box::new(AuthService {
                state: Arc::clone(&state),
                queued: Vec::new(),
                next_auth_id: 1,
            }),
        );
        self.state = Some(state);
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        self.state = None;
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, host: &mut ServiceHost, op: &str, args: &Value) -> Result<Value> {
        let Some(shared) = self.state.as_ref().cloned() else {
            return Err(rejected(format!(
                "module is off, so {op:?} was not applied"
            )));
        };
        match op {
            "state" => Ok(state_of(&shared).to_json()),
            "set" => {
                let Some(map) = args.as_object() else {
                    return Err(rejected("`set` needs an object of scripted fields"));
                };
                let mut state = state_of(&shared);
                for (key, value) in map {
                    match key.as_str() {
                        "can_authenticate" => {
                            let Some(text) = value.as_str() else {
                                return Err(rejected("can_authenticate must be a string"));
                            };
                            state.can_authenticate = parse_can_authenticate(text)?;
                        }
                        "enrolled" => {
                            let Some(flag) = value.as_bool() else {
                                return Err(rejected("enrolled must be a boolean"));
                            };
                            state.enrolled = flag;
                        }
                        other => {
                            return Err(rejected(format!(
                                "unknown scripted field {other:?}; this module accepts can_authenticate and enrolled"
                            )))
                        }
                    }
                }
                Ok(state.to_json())
            }
            "auth" => {
                let Some(result) = args.get("result").and_then(Value::as_str) else {
                    return Err(rejected(
                        "auth needs {result: \"success\"|\"failed\"|\"error\"|\"cancel\"|\"lockout\"}",
                    ));
                };
                let pending = {
                    let mut state = state_of(&shared);
                    let auth = state.pending.pop();
                    if let Some(auth) = &auth {
                        // The request is answered from the scripted control op,
                        // so the reference the service held on its receiver
                        // goes with it.
                        state.refs.release(BinderId(auth.receiver));
                    }
                    auth
                };
                let Some(auth) = pending else {
                    return Err(rejected(
                        "auth: no pending BiometricPrompt.authenticate call; call authenticate() in the app first",
                    ));
                };
                let delivery = match result {
                    "success" => receiver_delivery(
                        auth.receiver,
                        RECEIVER_SUCCEEDED,
                        "onAuthenticationSucceeded",
                        &[TYPE_FINGERPRINT],
                    ),
                    "failed" => {
                        receiver_delivery(auth.receiver, RECEIVER_FAILED, "onAuthenticationFailed", &[])
                    }
                    "cancel" => error_delivery(
                        auth.receiver,
                        TYPE_FINGERPRINT,
                        BIOMETRIC_ERROR_USER_CANCELED,
                        0,
                    ),
                    "lockout" => {
                        error_delivery(auth.receiver, TYPE_FINGERPRINT, BIOMETRIC_ERROR_LOCKOUT, 0)
                    }
                    "error" => {
                        let modality = parse_modality(args.get("modality"))?;
                        let error = parse_biometric_error(args.get("error"))?;
                        let vendor = args
                            .get("vendor_code")
                            .and_then(Value::as_i64)
                            .and_then(|v| i32::try_from(v).ok())
                            .unwrap_or(0);
                        error_delivery(auth.receiver, modality, error, vendor)
                    }
                    other => {
                        return Err(rejected(format!(
                            "unknown auth result {other:?}; use success, failed, error, cancel or lockout"
                        )))
                    }
                };
                host.queue_delivery(delivery);
                let mut state = state_of(&shared);
                state.answered.push(result.to_string());
                Ok(json!({
                    "result": result,
                    "auth_id": auth.auth_id,
                    "operation_id": auth.operation_id,
                    "package": auth.package,
                    "prompt_ui": "none — ACOS shows no biometric dialog (window layer stack gap)",
                }))
            }
            "reset" => {
                let mut state = state_of(&shared);
                state.pending.clear();
                let _ = state.refs.release_all();
                state.answered.clear();
                state.cancels = 0;
                state.unsupported.clear();
                Ok(json!({"reset": true}))
            }
            other => Err(rejected(format!(
                "unknown control op {other:?}; this module implements {}",
                CONTROL_OPS
                    .iter()
                    .map(|op| op.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    fn report(&self) -> ModuleReport {
        let mut report = ModuleReport::of(manifest_ref(), self.profile);
        if self.profile.is_attached() {
            let answered = self
                .state
                .as_ref()
                .map(|state| state_of(state).answered.len())
                .unwrap_or(0);
            report.reason = Some(format!(
                "{} profile: scripted canAuthenticate and BiometricPrompt result ({answered} answered); no biometric hardware, no enrolment store, no `fingerprint` HAL binder; no prompt UI is shown (the window layer stack is not built), so a visible dialog is never claimed; CryptoObject operation ids are recorded, never unlocked",
                self.profile
            ));
        }
        report
    }
}
