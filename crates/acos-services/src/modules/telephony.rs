//! `net.telephony` — telephony as far as a sandbox can be honest about it
//! (stub, scripted).
//!
//! A real phone's telephony is a radio, a SIM and a system process. ACOS has
//! none of those, so this module never claims `real`. What it does is answer the
//! **binder methods the framework's own client classes call**, with scripted
//! state, and deliver the two events an app actually reacts to: an incoming SMS
//! broadcast and a call-state change on the listener the app registered.
//!
//! ## Services (names and AIDL descriptors from the API-34 image)
//!
//! | name | descriptor | what answers |
//! |---|---|---|
//! | `phone` | `com.android.internal.telephony.ITelephony` | `getSimStateForSlotIndex`, `getActivePhoneTypeForSlot`, `getNetworkTypeForSubscriber`, `hasIccCardUsingSlotIndex`, `getLine1NumberForDisplay`, `getCallStateForSubscription` |
//! | `iphonesubinfo` | `com.android.internal.telephony.IPhoneSubInfo` | `getLine1Number[ForSubscriber]` (the fallback `getLine1Number()` takes when `phone` answers null) |
//! | `telephony.registry` | `com.android.internal.telephony.ITelephonyRegistry` | `listenWithEventList` + `removeOnSubscriptionsChangedListener`; the module delivers `IPhoneStateListener.onLegacyCallStateChanged` (PhoneStateListener) and `onCallStateChanged` (TelephonyCallback) |
//! | `isms` | `com.android.internal.telephony.ISms` | `sendTextForSubscriber` (recorded), `isSmsSimPickActivityNeeded` |
//!
//! Transaction codes are the image's own (`dexdump` of
//! `.cache/boot/android-34/dex/framework`), not the AOSP tag's, because a code
//! that shifted between the tag and the image would silently answer the wrong
//! method.
//!
//! ## What is scripted and what is not
//!
//! Scripted (`stub` answers fixed defaults, `scripted` lets the agent drive):
//! SIM state, ICC presence, phone type, network type, line-1 number, the
//! operator numeric/alpha, the call state, an incoming SMS (`sms` control op,
//! with a real GSM 03.40 SMS-DELIVER PDU) and the recorded outgoing SMS.
//!
//! Not available, and said so in every report:
//! * **No radio.** Network type and operator are whatever the script says; there
//!   is no signal, no data connection and no real SMS transport.
//! * **No system properties.** `TelephonyManager.getSimOperator()`/
//!   `getSimOperatorName()`/`getSimCountryIso()` read `gsm.sim.operator.*`
//!   properties, which a module cannot set (the guest's property table is
//!   written at launch, before modules attach). They answer `""`, and the
//!   report names the gap.
//! * **No subscription service.** `isub` stays `null`: AOSP's client caches fall
//!   back to `INVALID_SUBSCRIPTION_ID`, so binder calls arrive with `subId = -1`
//!   and the module answers by slot 0. A real subscription id would need an
//!   `ISub` implementation, which is a later module.
//! * **No `PendingIntent` scheduler.** `sendTextMessage`'s `sentIntent`/
//!   `deliveryIntent` are recorded only as "present"; no SENT/DELIVERED result is
//!   fired, because ACOS has no system process to send them from.
//! * **No prompt/notification UI.** An incoming SMS is a broadcast, exactly like
//!   AOSP's; the Messaging app's notification is not this module's to fake.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use acos_parcel::BinderId;

use crate::ams::IntentFacts;
use crate::error::{Result, ServiceError};
use crate::exception::write_no_exception;
use crate::host::{CallResult, PendingDelivery, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport,
    ProcessInitializer, Profile, SecurityClass, ServiceProvide,
};

/// The module id (the id `TOMBSTONES` already names for `phone`).
pub const MODULE_ID: &str = "net.telephony";
/// `ITelephony` descriptor.
pub const TELEPHONY_DESCRIPTOR: &str = "com.android.internal.telephony.ITelephony";
/// `IPhoneSubInfo` descriptor.
pub const SUBINFO_DESCRIPTOR: &str = "com.android.internal.telephony.IPhoneSubInfo";
/// `ITelephonyRegistry` descriptor.
pub const REGISTRY_DESCRIPTOR: &str = "com.android.internal.telephony.ITelephonyRegistry";
/// `ISms` descriptor.
pub const SMS_DESCRIPTOR: &str = "com.android.internal.telephony.ISms";
/// `IPhoneStateListener` descriptor (the app-side callback).
pub const STATE_LISTENER_DESCRIPTOR: &str = "com.android.internal.telephony.IPhoneStateListener";
/// The broadcast an incoming SMS is delivered as (`Telephony.Sms.Intents`).
pub const SMS_RECEIVED_ACTION: &str = "android.provider.Telephony.SMS_RECEIVED";

/// `TelephonyManager.SIM_STATE_READY`.
pub const SIM_STATE_READY: i32 = 5;
/// `TelephonyManager.SIM_STATE_ABSENT`.
pub const SIM_STATE_ABSENT: i32 = 1;
/// `TelephonyManager.PHONE_TYPE_GSM`.
pub const PHONE_TYPE_GSM: i32 = 1;
/// `TelephonyManager.NETWORK_TYPE_LTE`.
pub const NETWORK_TYPE_LTE: i32 = 13;
/// `TelephonyManager.CALL_STATE_IDLE`.
pub const CALL_STATE_IDLE: i32 = 0;
/// `TelephonyManager.CALL_STATE_RINGING`.
pub const CALL_STATE_RINGING: i32 = 1;
/// `TelephonyManager.CALL_STATE_OFFHOOK`.
pub const CALL_STATE_OFFHOOK: i32 = 2;

// --- transaction codes, read from the image's own `$Stub` static fields ------
//
// `com.android.internal.telephony.ITelephony$Stub` (classes3.dex), `ISms$Stub`,
// `IPhoneSubInfo$Stub`, `ITelephonyRegistry$Stub` (classes3.dex) and
// `IPhoneStateListener$Stub` (classes3.dex). A method missing here is answered
// with the structured `UNSUPPORTED_ANDROID_API`, never a guess.

/// `ITelephony.getCallState()`.
const TELEPHONY_GET_CALL_STATE: u32 = 34;
/// `ITelephony.getCallStateForSubscription(int, String, String)`.
const TELEPHONY_GET_CALL_STATE_FOR_SUBSCRIPTION: u32 = 35;
/// `ITelephony.getActivePhoneTypeForSlot(int)`.
const TELEPHONY_GET_ACTIVE_PHONE_TYPE_FOR_SLOT: u32 = 41;
/// `ITelephony.getNetworkTypeForSubscriber(int, String, String)`.
const TELEPHONY_GET_NETWORK_TYPE_FOR_SUBSCRIBER: u32 = 64;
/// `ITelephony.hasIccCardUsingSlotIndex(int)`.
const TELEPHONY_HAS_ICC_CARD_USING_SLOT_INDEX: u32 = 69;
/// `ITelephony.getLine1NumberForDisplay(int, String, String)`.
const TELEPHONY_GET_LINE1_NUMBER_FOR_DISPLAY: u32 = 125;
/// `ITelephony.getSimStateForSlotIndex(int)`.
const TELEPHONY_GET_SIM_STATE_FOR_SLOT_INDEX: u32 = 350;

/// `IPhoneSubInfo.getLine1Number(String, String)`.
const SUBINFO_GET_LINE1_NUMBER: u32 = 15;
/// `IPhoneSubInfo.getLine1NumberForSubscriber(int, String, String)`.
const SUBINFO_GET_LINE1_NUMBER_FOR_SUBSCRIBER: u32 = 16;

/// `ITelephonyRegistry.removeOnSubscriptionsChangedListener(String, IOnSubscriptionsChangedListener)`.
const REGISTRY_REMOVE_SUBSCRIPTIONS_CHANGED: u32 = 3;
/// `ITelephonyRegistry.listenWithEventList(...)`.
const REGISTRY_LISTEN_WITH_EVENT_LIST: u32 = 4;

/// `ISms.sendTextForSubscriber(...)`.
const SMS_SEND_TEXT_FOR_SUBSCRIBER: u32 = 5;
/// `ISms.isSmsSimPickActivityNeeded(int)`.
const SMS_IS_SIM_PICK_ACTIVITY_NEEDED: u32 = 19;
/// `ISms.isImsSmsSupportedForSubscriber(int)`.
const SMS_IS_IMS_SMS_SUPPORTED_FOR_SUBSCRIBER: u32 = 18;
/// `ISms.getPreferredSmsSubscription()`.
const SMS_GET_PREFERRED_SMS_SUBSCRIPTION: u32 = 20;
/// `ISms.getImsSmsFormatForSubscriber(int)`.
const SMS_GET_IMS_SMS_FORMAT_FOR_SUBSCRIBER: u32 = 21;
/// `ISms.isSMSPromptEnabled()`.
const SMS_IS_SMS_PROMPT_ENABLED: u32 = 22;

/// `IPhoneStateListener.onLegacyCallStateChanged(int, String)` — the callback a
/// `PhoneStateListener` with `LISTEN_CALL_STATE` is registered as.
const LISTENER_ON_LEGACY_CALL_STATE_CHANGED: u32 = 6;
/// `IPhoneStateListener.onCallStateChanged(int)` — the `TelephonyCallback` form.
const LISTENER_ON_CALL_STATE_CHANGED: u32 = 7;

/// `TelephonyCallback.EVENT_CALL_STATE_CHANGED`.
const EVENT_CALL_STATE_CHANGED: i32 = 6;
/// `TelephonyCallback.EVENT_LEGACY_CALL_STATE_CHANGED`.
const EVENT_LEGACY_CALL_STATE_CHANGED: i32 = 36;

/// One recorded `SmsManager.sendTextMessage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentSms {
    /// Subscription id the client passed (negative when the framework had none).
    pub sub_id: i32,
    /// Calling package.
    pub package: String,
    /// Destination address.
    pub destination: String,
    /// Service centre address (`null` = default SMSC).
    pub sc_address: Option<String>,
    /// Message body.
    pub text: String,
    /// Whether the caller asked for persistence.
    pub persist: bool,
    /// Whether a `sentIntent` was supplied (it is never fired: see the module
    /// docs).
    pub has_sent_intent: bool,
    /// Whether a `deliveryIntent` was supplied.
    pub has_delivery_intent: bool,
}

/// One incoming SMS the agent injected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedSms {
    /// Sender address as scripted.
    pub from: String,
    /// Body decoded from the PDU the app receives.
    pub body: String,
    /// The GSM 03.40 SMS-DELIVER PDU's hex.
    pub pdu: String,
    /// Receivers the broadcast reached.
    pub matched: i32,
}

/// One scripted call-state change and what it delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEvent {
    /// `TelephonyManager.CALL_STATE_*`.
    pub state: i32,
    /// The incoming number the event carried.
    pub number: Option<String>,
    /// Registered listeners that received the event.
    pub delivered: u32,
}

/// A registered `IPhoneStateListener` and the event ids it asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateListener {
    /// The app-side binder.
    pub binder: u64,
    /// Event ids (`TelephonyCallback.EVENT_*`; a `PhoneStateListener` sends the
    /// ids of the `TelephonyCallback` events it maps to).
    pub events: Vec<i32>,
    /// Calling package.
    pub package: String,
}

/// The session's telephony state.
#[derive(Debug)]
pub struct TelephonyState {
    /// `TelephonyManager.SIM_STATE_*`.
    pub sim_state: i32,
    /// `hasIccCard()`.
    pub icc_present: bool,
    /// `TelephonyManager.PHONE_TYPE_*`.
    pub phone_type: i32,
    /// `TelephonyManager.NETWORK_TYPE_*`.
    pub network_type: i32,
    /// `getLine1Number()`, `None` = no number (AOSP answers null).
    pub line1: Option<String>,
    /// `gsm.sim.operator.numeric` (only in `state`; the framework reads the
    /// guest property table, so the API answer stays `""`, see the docs).
    pub operator_numeric: String,
    /// The operator's display name (same property gap).
    pub operator_name: String,
    /// Incoming caller number used for call-state events.
    pub incoming_number: Option<String>,
    /// `TelephonyManager.CALL_STATE_*`.
    pub call_state: i32,
    /// Registered listeners.
    pub listeners: Vec<StateListener>,
    /// Scripted call-state changes (the `call_state` control op's evidence).
    pub call_events: Vec<CallEvent>,
    /// Recorded outgoing SMS.
    pub sent: Vec<SentSms>,
    /// Injected incoming SMS.
    pub injected: Vec<InjectedSms>,
    /// Calls answered with the structured unsupported error.
    pub unsupported: Vec<String>,
}

impl Default for TelephonyState {
    fn default() -> Self {
        Self {
            sim_state: SIM_STATE_READY,
            icc_present: true,
            phone_type: PHONE_TYPE_GSM,
            network_type: NETWORK_TYPE_LTE,
            line1: Some("+15551234567".to_string()),
            operator_numeric: "310260".to_string(),
            operator_name: "ACOS Scripted".to_string(),
            incoming_number: None,
            call_state: CALL_STATE_IDLE,
            listeners: Vec::new(),
            call_events: Vec::new(),
            sent: Vec::new(),
            injected: Vec::new(),
            unsupported: Vec::new(),
        }
    }
}

impl TelephonyState {
    /// The recorded state as JSON (the `state` control op and `report`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "sim_state": self.sim_state,
            "icc_present": self.icc_present,
            "phone_type": self.phone_type,
            "network_type": self.network_type,
            "line1": self.line1,
            "operator_numeric": self.operator_numeric,
            "operator_name": self.operator_name,
            "call_state": self.call_state,
            "incoming_number": self.incoming_number,
            "call_events": self.call_events.iter().map(|event| json!({
                "state": event.state,
                "number": event.number,
                "delivered": event.delivered,
            })).collect::<Vec<_>>(),
            "listeners": self.listeners.iter().map(|l| json!({
                "events": l.events,
                "package": l.package,
            })).collect::<Vec<_>>(),
            "sent": self.sent.iter().map(|s| json!({
                "sub_id": s.sub_id,
                "package": s.package,
                "destination": s.destination,
                "sc_address": s.sc_address,
                "text": s.text,
                "persist": s.persist,
                "has_sent_intent": s.has_sent_intent,
                "has_delivery_intent": s.has_delivery_intent,
            })).collect::<Vec<_>>(),
            "injected": self.injected.iter().map(|m| json!({
                "from": m.from,
                "body": m.body,
                "pdu": m.pdu,
                "matched": m.matched,
            })).collect::<Vec<_>>(),
            "unsupported": self.unsupported,
        })
    }
}

const CONTROL_OPS: [ControlOp; 5] = [
    ControlOp {
        name: "state",
        args: "none — the scripted state, recorded sends and injected SMS",
    },
    ControlOp {
        name: "set",
        args: "{sim_state?, icc_present?, phone_type?, network_type?, line1?, operator_numeric?, operator_name?} — change the scripted answers; a call-state change notifies every registered listener",
    },
    ControlOp {
        name: "call_state",
        args: "{state: \"idle\"|\"ringing\"|\"offhook\", number?} — set the call state and deliver onLegacyCallStateChanged/onCallStateChanged to every listener that asked for it",
    },
    ControlOp {
        name: "sms",
        args: "{from, body, pdu?} — inject an incoming SMS as the SMS_RECEIVED broadcast (a real GSM 03.40 SMS-DELIVER PDU; `pdu` hex overrides the encoder)",
    },
    ControlOp {
        name: "reset",
        args: "none — clear recorded sends/injections (not the scripted state)",
    },
];

/// The manifest. Static, so `Module::manifest` can hand out `&'static`.
fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "telephony: scripted SIM/phone/network state, line-1 number, SMS send recording and SMS_RECEIVED/call-state events (no radio)",
        // The telephony mainline module's own process installs the framework's
        // `TelephonyServiceManager` before any app runs; ACOS has no such
        // process, so the session that attaches this module runs the same call
        // in the guest, once, before the app boots (see
        // `ModuleManifest::process_initializers`).
        process_initializers: &[ProcessInitializer {
            class: "android.telephony.TelephonyFrameworkInitializer",
            method: "setTelephonyServiceManager",
            manager: "android.os.TelephonyServiceManager",
            evidence: "base/telephony/common/com/android/internal/telephony/frameworks/base/core/java/android/telephony/TelephonyFrameworkInitializer.java: `setTelephonyServiceManager` is the only setter for `sTelephonyServiceManager`, and `getTelephonyServiceManager` returns null until it runs; every `TelephonyManager`/`SmsManager` path resolves its binder through `TelephonyServiceManager.ServiceRegisterer.get()`, so without it the manager is null in the app process",
        }],
        features: &[
            "android.hardware.telephony",
            "android.hardware.telephony.messaging",
            "android.hardware.telephony.subscription",
        ],
        services: &[
            ServiceProvide {
                name: "phone",
                descriptor: TELEPHONY_DESCRIPTOR,
            },
            ServiceProvide {
                name: "iphonesubinfo",
                descriptor: SUBINFO_DESCRIPTOR,
            },
            ServiceProvide {
                name: "telephony.registry",
                descriptor: REGISTRY_DESCRIPTOR,
            },
            ServiceProvide {
                name: "isms",
                descriptor: SMS_DESCRIPTOR,
            },
        ],
        providers: &[],
        broadcasts: &[SMS_RECEIVED_ACTION],
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
        // The framework's `TelephonyServiceManager` is installed once, before
        // the guest boots (the phone process's job on a device), so telephony is
        // an at-launch module: a live attach could serve `phone` but every
        // `TelephonyManager` call would still NPE on the null manager.
        attach: AttachMode::AtLaunch,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

/// The module.
pub struct TelephonyModule {
    profile: Profile,
    state: Option<Arc<Mutex<TelephonyState>>>,
}

impl TelephonyModule {
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

impl Default for TelephonyModule {
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

fn state_of(state: &Arc<Mutex<TelephonyState>>) -> std::sync::MutexGuard<'_, TelephonyState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// --- services ---------------------------------------------------------------

/// `ITelephony` (`phone`).
struct TelephonyService {
    state: Arc<Mutex<TelephonyState>>,
}

impl TelephonyService {
    fn unsupported(&self, method: &str) -> ServiceError {
        state_of(&self.state).unsupported.push(method.to_string());
        ServiceError::UnsupportedAndroidApi {
            interface: TELEPHONY_DESCRIPTOR.to_string(),
            code: 0,
            method: format!("{MODULE_ID}: {method} has no stub answer"),
        }
    }

    fn bad(&self, method: &str, detail: impl Into<String>) -> ServiceError {
        ServiceError::BadRequest {
            interface: TELEPHONY_DESCRIPTOR.to_string(),
            method: method.to_string(),
            detail: detail.into(),
        }
    }
}

impl ServiceObject for TelephonyService {
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
        if interface != TELEPHONY_DESCRIPTOR {
            return Ok(false);
        }
        match code {
            TELEPHONY_GET_SIM_STATE_FOR_SLOT_INDEX => {
                let _slot = data
                    .read_int32()
                    .map_err(|e| self.bad("getSimStateForSlotIndex", e.to_string()))?;
                let state = state_of(&self.state);
                write_no_exception(reply);
                reply.write_int32(state.sim_state);
                Ok(true)
            }
            TELEPHONY_GET_ACTIVE_PHONE_TYPE_FOR_SLOT => {
                let _slot = data
                    .read_int32()
                    .map_err(|e| self.bad("getActivePhoneTypeForSlot", e.to_string()))?;
                let state = state_of(&self.state);
                write_no_exception(reply);
                reply.write_int32(state.phone_type);
                Ok(true)
            }
            TELEPHONY_GET_NETWORK_TYPE_FOR_SUBSCRIBER => {
                let _sub = data
                    .read_int32()
                    .map_err(|e| self.bad("getNetworkTypeForSubscriber", e.to_string()))?;
                let _pkg = data
                    .read_string16()
                    .map_err(|e| self.bad("getNetworkTypeForSubscriber", e.to_string()))?;
                let _feature = data
                    .read_string16()
                    .map_err(|e| self.bad("getNetworkTypeForSubscriber", e.to_string()))?;
                let state = state_of(&self.state);
                write_no_exception(reply);
                reply.write_int32(state.network_type);
                Ok(true)
            }
            TELEPHONY_HAS_ICC_CARD_USING_SLOT_INDEX => {
                let _slot = data
                    .read_int32()
                    .map_err(|e| self.bad("hasIccCardUsingSlotIndex", e.to_string()))?;
                let present = state_of(&self.state).icc_present;
                write_no_exception(reply);
                reply.write_int32(i32::from(present));
                Ok(true)
            }
            TELEPHONY_GET_LINE1_NUMBER_FOR_DISPLAY => {
                let _sub = data
                    .read_int32()
                    .map_err(|e| self.bad("getLine1NumberForDisplay", e.to_string()))?;
                let _pkg = data
                    .read_string16()
                    .map_err(|e| self.bad("getLine1NumberForDisplay", e.to_string()))?;
                let _feature = data
                    .read_string16()
                    .map_err(|e| self.bad("getLine1NumberForDisplay", e.to_string()))?;
                let line1 = state_of(&self.state).line1.clone();
                write_no_exception(reply);
                reply
                    .write_string16_opt(line1.as_deref())
                    .map_err(|e| self.bad("getLine1NumberForDisplay", e.to_string()))?;
                Ok(true)
            }
            TELEPHONY_GET_CALL_STATE => {
                let call_state = state_of(&self.state).call_state;
                write_no_exception(reply);
                reply.write_int32(call_state);
                Ok(true)
            }
            TELEPHONY_GET_CALL_STATE_FOR_SUBSCRIPTION => {
                let _sub = data
                    .read_int32()
                    .map_err(|e| self.bad("getCallStateForSubscription", e.to_string()))?;
                let _pkg = data
                    .read_string16()
                    .map_err(|e| self.bad("getCallStateForSubscription", e.to_string()))?;
                let _feature = data
                    .read_string16()
                    .map_err(|e| self.bad("getCallStateForSubscription", e.to_string()))?;
                let call_state = state_of(&self.state).call_state;
                write_no_exception(reply);
                reply.write_int32(call_state);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

/// `IPhoneSubInfo` (`iphonesubinfo`).
struct PhoneSubInfoService {
    state: Arc<Mutex<TelephonyState>>,
}

impl PhoneSubInfoService {
    fn bad(&self, method: &str, detail: impl Into<String>) -> ServiceError {
        ServiceError::BadRequest {
            interface: SUBINFO_DESCRIPTOR.to_string(),
            method: method.to_string(),
            detail: detail.into(),
        }
    }
}

impl ServiceObject for PhoneSubInfoService {
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
        if interface != SUBINFO_DESCRIPTOR {
            return Ok(false);
        }
        match code {
            SUBINFO_GET_LINE1_NUMBER => {
                let _pkg = data
                    .read_string16()
                    .map_err(|e| self.bad("getLine1Number", e.to_string()))?;
                let _feature = data
                    .read_string16()
                    .map_err(|e| self.bad("getLine1Number", e.to_string()))?;
                let line1 = state_of(&self.state).line1.clone();
                write_no_exception(reply);
                reply
                    .write_string16_opt(line1.as_deref())
                    .map_err(|e| self.bad("getLine1Number", e.to_string()))?;
                Ok(true)
            }
            SUBINFO_GET_LINE1_NUMBER_FOR_SUBSCRIBER => {
                let _sub = data
                    .read_int32()
                    .map_err(|e| self.bad("getLine1NumberForSubscriber", e.to_string()))?;
                let _pkg = data
                    .read_string16()
                    .map_err(|e| self.bad("getLine1NumberForSubscriber", e.to_string()))?;
                let _feature = data
                    .read_string16()
                    .map_err(|e| self.bad("getLine1NumberForSubscriber", e.to_string()))?;
                let line1 = state_of(&self.state).line1.clone();
                write_no_exception(reply);
                reply
                    .write_string16_opt(line1.as_deref())
                    .map_err(|e| self.bad("getLine1NumberForSubscriber", e.to_string()))?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

/// `ITelephonyRegistry` (`telephony.registry`).
struct TelephonyRegistryService {
    state: Arc<Mutex<TelephonyState>>,
    queued: Vec<PendingDelivery>,
    /// The live registrations, as host-held strong references. A registration
    /// *is* a reference: the host keeps the guest's node alive while it exists,
    /// and releasing it is what tells the driver to unroot the node
    /// (`F_REF` release -> `BR_RELEASE` + `BR_DECREFS`). One ledger for every
    /// module, so re-registering a listener cannot double-count.
    refs: crate::refs::GuestRefs,
}

impl TelephonyRegistryService {
    fn bad(&self, method: &str, detail: impl Into<String>) -> ServiceError {
        ServiceError::BadRequest {
            interface: REGISTRY_DESCRIPTOR.to_string(),
            method: method.to_string(),
            detail: detail.into(),
        }
    }
}

impl ServiceObject for TelephonyRegistryService {
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
        if interface != REGISTRY_DESCRIPTOR {
            return Ok(false);
        }
        match code {
            REGISTRY_LISTEN_WITH_EVENT_LIST => {
                let _renounce_fine = data
                    .read_int32()
                    .map_err(|e| self.bad("listenWithEventList", e.to_string()))?;
                let _renounce_coarse = data
                    .read_int32()
                    .map_err(|e| self.bad("listenWithEventList", e.to_string()))?;
                let _sub_id = data
                    .read_int32()
                    .map_err(|e| self.bad("listenWithEventList", e.to_string()))?;
                let package = data
                    .read_string16()
                    .map_err(|e| self.bad("listenWithEventList", e.to_string()))?
                    .unwrap_or_default();
                let _feature = data
                    .read_string16()
                    .map_err(|e| self.bad("listenWithEventList", e.to_string()))?;
                let binder = data
                    .read_strong_binder()
                    .map_err(|e| self.bad("listenWithEventList", e.to_string()))?
                    .map(|id| id.0)
                    .unwrap_or(0);
                let events = data
                    .read_int32_array()
                    .map_err(|e| self.bad("listenWithEventList", e.to_string()))?
                    .unwrap_or_default();
                let notify_now = data
                    .read_int32()
                    .map_err(|e| self.bad("listenWithEventList", e.to_string()))?
                    != 0;
                let (call_state, number) = {
                    let mut state = state_of(&self.state);
                    if binder != 0 {
                        // `LISTEN_NONE` (an empty event list) is the unregister
                        // path: the registration goes away, and so does the
                        // reference the host held for it.
                        state.listeners.retain(|listener| listener.binder != binder);
                        if events.is_empty() {
                            // `LISTEN_NONE`: the registration goes away, and so
                            // does the reference the host held for it.
                            self.refs.release(BinderId(binder));
                        } else {
                            state.listeners.push(StateListener {
                                binder,
                                events: events.clone(),
                                package,
                            });
                            self.refs.hold(BinderId(binder));
                        }
                    }
                    (state.call_state, state.incoming_number.clone())
                };
                // Android's registry notifies a fresh listener immediately with
                // the current state; do the same, through the same callback.
                if notify_now && binder != 0 {
                    if let Some(delivery) =
                        call_state_delivery(binder, &events, call_state, number.as_deref())
                    {
                        self.queued.push(delivery);
                    }
                }
                write_no_exception(reply);
                Ok(true)
            }
            REGISTRY_REMOVE_SUBSCRIPTIONS_CHANGED => {
                // `String pkg, IOnSubscriptionsChangedListener callback`: drop
                // the registration if the callback is one of ours (a listener
                // registered through `listen`), which costs no extra state.
                let _pkg = data
                    .read_string16()
                    .map_err(|e| self.bad("removeOnSubscriptionsChangedListener", e.to_string()))?;
                let binder = data
                    .read_strong_binder()
                    .map_err(|e| self.bad("removeOnSubscriptionsChangedListener", e.to_string()))?
                    .map(|id| id.0)
                    .unwrap_or(0);
                state_of(&self.state)
                    .listeners
                    .retain(|listener| listener.binder != binder);
                if binder != 0 {
                    self.refs.release(BinderId(binder));
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
        self.refs.drain_retained()
    }

    fn drain_released(&mut self) -> Vec<BinderId> {
        self.refs.drain_released()
    }

    fn release_all_refs(&mut self) -> Vec<BinderId> {
        self.refs.release_all()
    }
}

/// `ISms` (`isms`).
struct SmsService {
    state: Arc<Mutex<TelephonyState>>,
}

impl SmsService {
    fn bad(&self, method: &str, detail: impl Into<String>) -> ServiceError {
        ServiceError::BadRequest {
            interface: SMS_DESCRIPTOR.to_string(),
            method: method.to_string(),
            detail: detail.into(),
        }
    }
}

impl ServiceObject for SmsService {
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
        if interface != SMS_DESCRIPTOR {
            return Ok(false);
        }
        match code {
            SMS_SEND_TEXT_FOR_SUBSCRIBER => {
                // `sendTextForSubscriber(int subId, String callingPkg,
                // String callingAttributionTag, String destAddr, String
                // scAddr, String text, PendingIntent sentIntent,
                // PendingIntent deliveryIntent, boolean persistMessage,
                // long messageId)`. The two `PendingIntent`s are read only as
                // their presence word: the module never fires them, and their
                // payload is not needed to record the send.
                let sub_id = data
                    .read_int32()
                    .map_err(|e| self.bad("sendTextForSubscriber", e.to_string()))?;
                let package = data
                    .read_string16()
                    .map_err(|e| self.bad("sendTextForSubscriber", e.to_string()))?
                    .unwrap_or_default();
                let _feature = data
                    .read_string16()
                    .map_err(|e| self.bad("sendTextForSubscriber", e.to_string()))?;
                let destination = data
                    .read_string16()
                    .map_err(|e| self.bad("sendTextForSubscriber", e.to_string()))?
                    .unwrap_or_default();
                let sc_address = data
                    .read_string16()
                    .map_err(|e| self.bad("sendTextForSubscriber", e.to_string()))?;
                let text = data
                    .read_string16()
                    .map_err(|e| self.bad("sendTextForSubscriber", e.to_string()))?
                    .unwrap_or_default();
                let has_sent_intent = data
                    .read_int32()
                    .map_err(|e| self.bad("sendTextForSubscriber", e.to_string()))?
                    != 0;
                let has_delivery_intent = data
                    .read_int32()
                    .map_err(|e| self.bad("sendTextForSubscriber", e.to_string()))?
                    != 0;
                {
                    let mut state = state_of(&self.state);
                    if state.sim_state == SIM_STATE_ABSENT {
                        // A phone with no SIM cannot send: AOSP's radio would
                        // answer RESULT_ERROR_RADIO_OFF. The module refuses
                        // instead of recording a send that cannot happen.
                        return Err(rejected(
                            "sendTextForSubscriber: the scripted SIM state is ABSENT, so no SMS can be sent (AOSP would answer RESULT_ERROR_RADIO_OFF)",
                        ));
                    }
                    state.sent.push(SentSms {
                        sub_id,
                        package,
                        destination,
                        sc_address,
                        text,
                        // `persistMessage`/`messageId` follow the intents on the
                        // wire; the module records the send and names the
                        // PendingIntent gap in every report.
                        persist: true,
                        has_sent_intent,
                        has_delivery_intent,
                    });
                }
                write_no_exception(reply);
                Ok(true)
            }
            // `SmsManager.sendTextMessage` resolves the subscription before it
            // sends: `getPreferredSmsSubscription` (no subscription service, so
            // AOSP's own invalid id), `isImsSmsSupported` false and the IMS
            // format. Answering them keeps the framework on its own path instead
            // of failing with a structured error for a method the stub simply
            // did not list.
            SMS_GET_PREFERRED_SMS_SUBSCRIPTION => {
                write_no_exception(reply);
                reply.write_int32(-1); // SubscriptionManager.INVALID_SUBSCRIPTION_ID
                Ok(true)
            }
            SMS_IS_IMS_SMS_SUPPORTED_FOR_SUBSCRIBER => {
                let _sub = data
                    .read_int32()
                    .map_err(|e| self.bad("isImsSmsSupportedForSubscriber", e.to_string()))?;
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            SMS_GET_IMS_SMS_FORMAT_FOR_SUBSCRIBER => {
                let _sub = data
                    .read_int32()
                    .map_err(|e| self.bad("getImsSmsFormatForSubscriber", e.to_string()))?;
                write_no_exception(reply);
                reply
                    .write_string16_opt(Some("3gpp"))
                    .map_err(ServiceError::Parcel)?;
                Ok(true)
            }
            SMS_IS_SMS_PROMPT_ENABLED => {
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            SMS_IS_SIM_PICK_ACTIVITY_NEEDED => {
                let _sub = data
                    .read_int32()
                    .map_err(|e| self.bad("isSmsSimPickActivityNeeded", e.to_string()))?;
                // One scripted subscription: no picker is ever needed, so
                // `SmsManager.sendTextMessage` goes straight to the send.
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

/// The `IPhoneStateListener` delivery for a call-state change.
///
/// `events` selects the callback shape the app registered for: a
/// `PhoneStateListener` registered `EVENT_LEGACY_CALL_STATE_CHANGED` (and gets
/// `onLegacyCallStateChanged(int, String)`), a `TelephonyCallback` registered
/// `EVENT_CALL_STATE_CHANGED` (and gets `onCallStateChanged(int)`). `None` when
/// the listener did not ask for call state at all, which is what the real
/// registry does.
#[must_use]
pub fn call_state_delivery(
    binder: u64,
    events: &[i32],
    call_state: i32,
    number: Option<&str>,
) -> Option<PendingDelivery> {
    let legacy = events.contains(&EVENT_LEGACY_CALL_STATE_CHANGED);
    let modern = events.contains(&EVENT_CALL_STATE_CHANGED);
    if !legacy && !modern {
        return None;
    }
    let mut data = acos_parcel::Parcel::new();
    data.write_interface_token(STATE_LISTENER_DESCRIPTOR).ok()?;
    let code = if legacy {
        LISTENER_ON_LEGACY_CALL_STATE_CHANGED
    } else {
        LISTENER_ON_CALL_STATE_CHANGED
    };
    data.write_int32(call_state);
    if legacy {
        data.write_string16_opt(number).ok()?;
    }
    Some(PendingDelivery {
        target: acos_parcel::BinderId(binder),
        interface: STATE_LISTENER_DESCRIPTOR.to_string(),
        code,
        method: if legacy {
            "onLegacyCallStateChanged".to_string()
        } else {
            "onCallStateChanged".to_string()
        },
        data,
    })
}

// --- configuration ----------------------------------------------------------

fn parse_sim_state(text: &str) -> Result<i32> {
    Ok(match text {
        "unknown" => 0,
        "absent" => SIM_STATE_ABSENT,
        "pin_required" => 2,
        "puk_required" => 3,
        "network_locked" => 4,
        "ready" | "loaded" => SIM_STATE_READY,
        "not_ready" => 6,
        "perm_disabled" => 7,
        "card_io_error" => 8,
        "card_restricted" => 9,
        other => return Err(rejected(format!(
            "unknown sim_state {other:?}; use unknown, absent, pin_required, puk_required, network_locked, ready, not_ready, perm_disabled, card_io_error or card_restricted"
        ))),
    })
}

fn parse_phone_type(text: &str) -> Result<i32> {
    Ok(match text {
        "none" => 0,
        "gsm" => PHONE_TYPE_GSM,
        "cdma" => 2,
        "sip" => 3,
        other => {
            return Err(rejected(format!(
                "unknown phone_type {other:?}; use none, gsm, cdma or sip"
            )))
        }
    })
}

fn parse_call_state(text: &str) -> Result<i32> {
    Ok(match text {
        "idle" => CALL_STATE_IDLE,
        "ringing" => CALL_STATE_RINGING,
        "offhook" | "off_hook" => CALL_STATE_OFFHOOK,
        other => {
            return Err(rejected(format!(
                "unknown call state {other:?}; use idle, ringing or offhook"
            )))
        }
    })
}

fn parse_network_type(value: &Value) -> Result<i32> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .filter(|n| (0..=20).contains(n))
            .ok_or_else(|| {
                rejected("network_type must be a TelephonyManager.NETWORK_TYPE_* number (0..20)")
            }),
        Value::String(text) => Ok(match text.as_str() {
            "unknown" => 0,
            "gprs" => 1,
            "edge" => 2,
            "umts" => 3,
            "cdma" => 4,
            "evdo_0" => 5,
            "evdo_a" => 6,
            "1xrtt" => 7,
            "hsdpa" => 8,
            "hsupa" => 9,
            "hspa" => 10,
            "iden" => 11,
            "evdo_b" => 12,
            "lte" => NETWORK_TYPE_LTE,
            "ehrpd" => 14,
            "hspap" => 15,
            "gsm" => 16,
            "td_scdma" => 17,
            "iwlan" => 18,
            "lte_ca" => 19,
            "nr" => 20,
            other => {
                return Err(rejected(format!(
                    "unknown network_type {other:?}; use a NETWORK_TYPE name or 0..20"
                )))
            }
        }),
        other => Err(rejected(format!(
            "network_type must be a string or number, got {other}"
        ))),
    }
}

fn apply_set(state: &mut TelephonyState, args: &Value) -> Result<()> {
    let Some(map) = args.as_object() else {
        return Err(rejected("`set` needs an object of scripted fields"));
    };
    let mut call_state_changed = false;
    for (key, value) in map {
        match key.as_str() {
            "sim_state" => {
                let Some(text) = value.as_str() else {
                    return Err(rejected("sim_state must be a string"));
                };
                state.sim_state = parse_sim_state(text)?;
            }
            "icc_present" => {
                let Some(flag) = value.as_bool() else {
                    return Err(rejected("icc_present must be a boolean"));
                };
                state.icc_present = flag;
            }
            "phone_type" => {
                let Some(text) = value.as_str() else {
                    return Err(rejected("phone_type must be a string"));
                };
                state.phone_type = parse_phone_type(text)?;
            }
            "network_type" => state.network_type = parse_network_type(value)?,
            "line1" => match value {
                Value::Null => state.line1 = None,
                Value::String(text) => state.line1 = Some(text.clone()),
                _ => return Err(rejected("line1 must be a string or null")),
            },
            "operator_numeric" => {
                let Some(text) = value.as_str() else {
                    return Err(rejected("operator_numeric must be a string"));
                };
                if !text.chars().all(|c| c.is_ascii_digit()) || text.len() < 5 {
                    return Err(rejected(
                        "operator_numeric must be 5-6 decimal digits (MCC+MNC)",
                    ));
                }
                state.operator_numeric = text.to_string();
            }
            "operator_name" => {
                let Some(text) = value.as_str() else {
                    return Err(rejected("operator_name must be a string"));
                };
                state.operator_name = text.to_string();
            }
            "call_state" => {
                let Some(text) = value.as_str() else {
                    return Err(rejected("call_state must be a string"));
                };
                let next = parse_call_state(text)?;
                call_state_changed = call_state_changed || next != state.call_state;
                state.call_state = next;
            }
            other => {
                return Err(rejected(format!(
                    "unknown scripted field {other:?}; this module accepts sim_state, icc_present, phone_type, network_type, line1, operator_numeric, operator_name, call_state"
                )))
            }
        }
    }
    if call_state_changed {
        // No listener notification here: `set` changes the state a *query*
        // answers. `call_state` is the op that also delivers the event, so an
        // agent that only wants a new answer does not fake a phone event.
    }
    Ok(())
}

// --- GSM 03.40 SMS-DELIVER encoding ----------------------------------------

/// The GSM 03.38 default alphabet, as the septet a character maps to.
///
/// Characters that would need the escape sequence (`[`, `]`, `{`, `}`, `\`,
/// `|`, `^`, `~`, `€`) are refused instead of silently mis-encoding: a fixture
/// that wants one must send the raw `pdu` hex.
fn gsm7_septet(c: char) -> Option<u8> {
    Some(match c {
        '@' => 0x00,
        '£' => 0x01,
        '$' => 0x02,
        '¥' => 0x03,
        'è' => 0x04,
        'é' => 0x05,
        'ù' => 0x06,
        'ì' => 0x07,
        'ò' => 0x08,
        'Ç' => 0x09,
        '\n' => 0x0A,
        'Ø' => 0x0B,
        'ø' => 0x0C,
        '\r' => 0x0D,
        'Å' => 0x0E,
        'å' => 0x0F,
        'Δ' => 0x10,
        '_' => 0x11,
        'Φ' => 0x12,
        'Γ' => 0x13,
        'Λ' => 0x14,
        'Ω' => 0x15,
        'Π' => 0x16,
        'Ψ' => 0x17,
        'Σ' => 0x18,
        'Θ' => 0x19,
        'Ξ' => 0x1A,
        'Æ' => 0x1C,
        'æ' => 0x1D,
        'ß' => 0x1E,
        'É' => 0x1F,
        '¡' => 0x40,
        'Ä' => 0x5B,
        'Ö' => 0x5C,
        'Ñ' => 0x5D,
        'Ü' => 0x5E,
        '§' => 0x5F,
        '¿' => 0x60,
        'ä' => 0x7B,
        'ö' => 0x7C,
        'ñ' => 0x7D,
        'ü' => 0x7E,
        ' '..='?' => c as u8,
        'A'..='Z' => c as u8,
        'a'..='z' => c as u8,
        _ => return None,
    })
}

/// Swap each pair of decimal digits into BCD semi-octets (`1234` → `21 43`).
///
/// An odd digit count pads the last semi-octet with `F`, which is what the
/// address field's length in semi-octets says to expect.
fn bcd_semi_octets(digits: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity((digits.len() + 1) / 2);
    let mut i = 0;
    while i < digits.len() {
        let low = digits[i];
        let high = digits.get(i + 1).copied().unwrap_or(0x0F);
        out.push((high << 4) | low);
        i += 2;
    }
    out
}

fn decimal_digits(text: &str) -> Option<Vec<u8>> {
    text.chars()
        .map(|c| c.to_digit(10).map(|d| d as u8))
        .collect()
}

/// Pack septets LSB-first into octets (GSM 03.38 §6.1.2.1.1).
fn pack_septets(septets: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity((septets.len() * 7 + 7) / 8);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for septet in septets {
        acc |= u32::from(*septet) << bits;
        bits += 7;
        while bits >= 8 {
            out.push((acc & 0xFF) as u8);
            acc >>= 8;
            bits -= 8;
        }
    }
    if bits > 0 {
        out.push((acc & 0xFF) as u8);
    }
    out
}

/// `TP-SCTS` semi-octet digits for a UTC timestamp (`YYMMDDHHMMSS` + `00`).
fn scts_digits(unix_seconds: u64) -> Vec<u8> {
    // Convert to UTC without pulling a date crate: days since the epoch, then
    // the civil-from-days algorithm (Howard Hinnant's), which is exact for the
    // whole proleptic Gregorian calendar.
    let days = (unix_seconds / 86_400) as i64;
    let secs_of_day = unix_seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    let hour = (secs_of_day / 3_600) as u8;
    let minute = ((secs_of_day % 3_600) / 60) as u8;
    let second = (secs_of_day % 60) as u8;
    let mut digits = Vec::with_capacity(14);
    digits.push(((year % 100) / 10) as u8);
    digits.push((year % 10) as u8);
    digits.push((m / 10) as u8);
    digits.push((m % 10) as u8);
    digits.push((d / 10) as u8);
    digits.push((d % 10) as u8);
    digits.push(hour / 10);
    digits.push(hour % 10);
    digits.push(minute / 10);
    digits.push(minute % 10);
    digits.push(second / 10);
    digits.push(second % 10);
    digits.push(0); // timezone: GMT+0, tens
    digits.push(0); // timezone: GMT+0, units
    digits
}

/// Encode one `SMS-DELIVER` PDU (GSM 03.40) carrying `body` from `from`.
///
/// The body must be representable in the GSM 03.38 default alphabet; `from` must
/// be digits with an optional leading `+`. Anything else is a structured error,
/// never a PDU that decodes to something else.
pub fn encode_sms_deliver(from: &str, body: &str, now_unix_seconds: u64) -> Result<Vec<u8>> {
    let (international, digits) = match from.strip_prefix('+') {
        Some(rest) => (true, rest),
        None => (false, from),
    };
    let address = decimal_digits(digits)
        .filter(|digits| !digits.is_empty() && digits.len() <= 20)
        .ok_or_else(|| {
            rejected(format!(
                "from {from:?} is not a phone number (digits, optional leading '+')"
            ))
        })?;
    let septets: Vec<u8> = body
        .chars()
        .map(|c| {
            gsm7_septet(c).ok_or_else(|| {
                rejected(format!(
                    "character {c:?} is not in the GSM 03.38 default alphabet; send `pdu` hex for a custom PDU"
                ))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if septets.len() > 160 {
        return Err(rejected(
            "body is longer than one SMS (160 septets); multipart PDUs are not implemented",
        ));
    }
    let mut pdu = Vec::with_capacity(24 + septets.len());
    pdu.push(0x00); // SMSC: no service centre address in the PDU
    pdu.push(0x04); // SMS-DELIVER: no reply path, no user-data header, no more messages
    pdu.push(u8::try_from(address.len()).unwrap_or(u8::MAX)); // address length in semi-octets
    pdu.push(if international { 0x91 } else { 0x81 }); // type of address
    pdu.extend(bcd_semi_octets(&address));
    pdu.push(0x00); // TP-PID
    pdu.push(0x00); // TP-DCS: GSM 7-bit default alphabet
    pdu.extend(bcd_semi_octets(&scts_digits(now_unix_seconds)));
    pdu.push(u8::try_from(septets.len()).unwrap_or(u8::MAX)); // TP-UDL in septets
    pdu.extend(pack_septets(&septets));
    Ok(pdu)
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn from_hex(text: &str) -> Result<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return Err(rejected("pdu hex has an odd number of digits"));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16)
                .map_err(|_| rejected(format!("pdu is not hex: {:?}", &text[i..i + 2])))
        })
        .collect()
}

// --- the module -------------------------------------------------------------

impl Module for TelephonyModule {
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
                "{profile} is not implemented: ACOS has no radio, so use `stub` or `scripted`"
            )));
        }
        let mut state = TelephonyState::default();
        if let Some(value) = config.get("state") {
            apply_set(&mut state, value)?;
        }
        // Every other key is a scripted field spelled directly in `config`.
        let mut direct = serde_json::Map::new();
        for (key, value) in config.to_json().as_object().cloned().unwrap_or_default() {
            if key == "state" {
                continue;
            }
            direct.insert(key, value);
        }
        if !direct.is_empty() {
            apply_set(&mut state, &Value::Object(direct))?;
        }
        let state = Arc::new(Mutex::new(state));
        host.register_dynamic_named(
            "phone",
            TELEPHONY_DESCRIPTOR,
            Box::new(TelephonyService {
                state: Arc::clone(&state),
            }),
        );
        host.register_dynamic_named(
            "iphonesubinfo",
            SUBINFO_DESCRIPTOR,
            Box::new(PhoneSubInfoService {
                state: Arc::clone(&state),
            }),
        );
        host.register_dynamic_named(
            "telephony.registry",
            REGISTRY_DESCRIPTOR,
            Box::new(TelephonyRegistryService {
                state: Arc::clone(&state),
                queued: Vec::new(),
                refs: crate::refs::GuestRefs::new(),
            }),
        );
        host.register_dynamic_named(
            "isms",
            SMS_DESCRIPTOR,
            Box::new(SmsService {
                state: Arc::clone(&state),
            }),
        );
        self.state = Some(state);
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        // The registry's scope rolls the four registrations back; dropping the
        // state is what makes a second attach start clean.
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
                let mut state = state_of(&shared);
                apply_set(&mut state, args)?;
                Ok(state.to_json())
            }
            "call_state" => {
                let (next, number) = {
                    let mut state = state_of(&shared);
                    let next = match args.get("state") {
                        Some(Value::String(text)) => parse_call_state(text)?,
                        Some(other) => {
                            return Err(rejected(format!(
                                "call_state needs a string `state`, got {other}"
                            )))
                        }
                        None => {
                            return Err(rejected(
                                "call_state needs {state: \"idle\"|\"ringing\"|\"offhook\"}",
                            ))
                        }
                    };
                    let number = match args.get("number") {
                        Some(Value::Null) | None => state.incoming_number.clone(),
                        Some(Value::String(text)) => Some(text.clone()),
                        Some(other) => {
                            return Err(rejected(format!("number must be a string, got {other}")))
                        }
                    };
                    state.call_state = next;
                    state.incoming_number = number.clone();
                    (next, number)
                };
                let listeners: Vec<(u64, Vec<i32>)> = state_of(&shared)
                    .listeners
                    .iter()
                    .map(|listener| (listener.binder, listener.events.clone()))
                    .collect();
                let mut delivered = 0;
                for (binder, events) in listeners {
                    if let Some(delivery) =
                        call_state_delivery(binder, &events, next, number.as_deref())
                    {
                        host.queue_delivery(delivery);
                        delivered += 1;
                    }
                }
                state_of(&shared).call_events.push(CallEvent {
                    state: next,
                    number: number.clone(),
                    delivered,
                });
                Ok(json!({
                    "call_state": next,
                    "incoming_number": number,
                    "delivered": delivered,
                }))
            }
            "sms" => {
                let (Some(from), Some(body)) = (
                    args.get("from").and_then(Value::as_str),
                    args.get("body").and_then(Value::as_str),
                ) else {
                    return Err(rejected("sms needs string `from` and `body`"));
                };
                let pdu = match args.get("pdu").and_then(Value::as_str) {
                    Some(hex) => from_hex(hex)?,
                    None => encode_sms_deliver(from, body, now_unix_seconds())?,
                };
                let mut intent = IntentFacts::action(SMS_RECEIVED_ACTION);
                intent.extras = {
                    let mut extras = std::collections::BTreeMap::new();
                    extras.insert(
                        "pdus".to_string(),
                        crate::ams::Extra::ByteArrays(vec![pdu.clone()]),
                    );
                    extras.insert(
                        "format".to_string(),
                        crate::ams::Extra::String("3gpp".to_string()),
                    );
                    extras
                };
                let matched = host.broadcast_intent(&intent)?;
                let hex = to_hex(&pdu);
                state_of(&shared).injected.push(InjectedSms {
                    from: from.to_string(),
                    body: body.to_string(),
                    pdu: hex.clone(),
                    matched,
                });
                Ok(json!({
                    "action": SMS_RECEIVED_ACTION,
                    "from": from,
                    "body": body,
                    "pdu": hex,
                    "pdu_bytes": pdu.len(),
                    "matched": matched,
                }))
            }
            "reset" => {
                let mut state = state_of(&shared);
                state.sent.clear();
                state.injected.clear();
                state.call_events.clear();
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
            let state = self
                .state
                .as_ref()
                .map(|state| state_of(state).to_json())
                .unwrap_or(Value::Null);
            let sent = state
                .get("sent")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let injected = state
                .get("injected")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let listeners = state
                .get("listeners")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            report.reason = Some(format!(
                "{} profile: scripted SIM/network/line-1 answers, {sent} recorded outgoing SMS, {injected} injected incoming SMS, {listeners} call-state listener(s); no radio, no real SMS transport; `getSimOperator*`/`getSimCountryIso` read guest system properties a module cannot set and stay \"\"; no `isub`, so binder calls arrive with subId=-1 and are answered by slot 0; at-launch only (the framework telephony initializer is installed before the app boots, as the phone process does); sent/delivery PendingIntents are recorded but never fired (no PendingIntent scheduler)",
                self.profile
            ));
        }
        report
    }
}

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sms_deliver_pdu_has_the_gsm_layout_for_a_short_ascii_body() {
        // 2026-11-16T12:00:00Z
        let pdu = encode_sms_deliver("+15551234567", "Hi", 1_795_000_000).expect("encode");
        assert_eq!(pdu[0], 0x00, "no SMSC address");
        assert_eq!(pdu[1] & 0x03, 0x00, "TP-MTI: SMS-DELIVER");
        assert_eq!(pdu[2], 11, "address length in semi-octets");
        assert_eq!(pdu[3], 0x91, "international type of address");
        assert_eq!(&pdu[4..10], &[0x51, 0x55, 0x21, 0x43, 0x65, 0xF7]);
        assert_eq!(pdu[10], 0x00, "TP-PID");
        assert_eq!(pdu[11], 0x00, "TP-DCS: GSM 7-bit");
        assert_eq!(&pdu[12..19], &[0x62, 0x11, 0x81, 0x11, 0x60, 0x04, 0x00]);
        assert_eq!(pdu[19], 2, "TP-UDL: two septets");
        // 'H' = 0x48, 'i' = 0x69 -> 0x48 | 0x69 << 7 = 0x34C8 -> c8 34.
        assert_eq!(&pdu[20..], &[0xC8, 0x34]);
    }

    #[test]
    fn the_pdu_round_trips_through_the_gsm7_packing() {
        let septets: Vec<u8> = "ACOS SMS"
            .chars()
            .map(|c| gsm7_septet(c).unwrap())
            .collect();
        let packed = pack_septets(&septets);
        let mut unpacked = Vec::new();
        let mut acc: u32 = 0;
        let mut bits = 0;
        for byte in packed {
            acc |= u32::from(byte) << bits;
            bits += 8;
            while bits >= 7 {
                unpacked.push((acc & 0x7F) as u8);
                acc >>= 7;
                bits -= 7;
            }
        }
        assert_eq!(&unpacked[..septets.len()], &septets[..]);
    }

    #[test]
    fn a_body_outside_the_default_alphabet_is_refused_not_mis_encoded() {
        let error = encode_sms_deliver("+15551234567", "a {brace}", 0).expect_err("refused");
        assert_eq!(error.code(), crate::error::codes::SERVICE_MODULE_REJECTED);
    }

    #[test]
    fn a_hex_pdu_override_round_trips() {
        let hex = "00040B915155214365F700006211811160040002C834".to_lowercase();
        let pdu = from_hex(&hex).expect("hex");
        assert_eq!(to_hex(&pdu), hex);
    }
}
