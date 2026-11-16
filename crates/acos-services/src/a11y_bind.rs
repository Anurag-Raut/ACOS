//! System -> app bind for an enabled `AccessibilityService` (MODA11Y2).
//!
//! AOSP's `AccessibilityManagerService` does four things the app's service
//! observes, and this module is the ACOS measure of all four:
//!
//! 1. it builds the bind `Intent` (`action =
//!    android.accessibilityservice.AccessibilityService`, the enabled
//!    component) and asks the app process to create and bind the service
//!    (`IApplicationThread.scheduleCreateService` /
//!    `scheduleBindService`);
//! 2. the app's real `ActivityThread` instantiates the class and runs the
//!    real `Service.onCreate`/`AccessibilityService.onServiceConnected` after
//!    3;
//! 3. `ActivityThread.handleBindService` reports the service binder back
//!    through `IActivityManager.publishService(token, intent, binder)`;
//! 4. the system calls `IAccessibilityServiceClient.init(connection,
//!    connectionId, windowToken)` on that binder, where `connection` is the
//!    service's handle on the system side (`IAccessibilityServiceConnection`).
//!
//! Only (1), (3) and (4) are host work; (2) is the framework, unchanged. Every
//! payload here is written by the framework's own field order
//! ([`crate::parcelable`] `Intent`, [`crate::pms::ServiceInfo`],
//! `CompatibilityInfo`), never hand-assembled bytes.
//!
//! The bind is a **feature inside the existing `PendingDelivery` + dynamic-node
//! design**: deliveries are the same `system -> app` queue every other
//! reverse call uses, and `IAccessibilityServiceConnection` is registered as a
//! dynamic node exactly like `IBulkCursor`/`IWindowSession`.

use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};

use crate::error::{Result, ServiceError};
use crate::host::{CallResult, PendingDelivery, ServiceObject};
use crate::misc::{service_info_value, A11yState};
use crate::pms::MetaDataEntry;
use crate::value::{Parcelable, Value};

/// The app-side process interface the bind is delivered to.
pub const APP_THREAD: &str = "android.app.IApplicationThread";
/// The accessibility-service action a device enables a service with.
pub const SERVICE_INTERFACE: &str = "android.accessibilityservice.AccessibilityService";
/// The service-side interface the system calls `init` on.
pub const SERVICE_CLIENT: &str = "android.accessibilityservice.IAccessibilityServiceClient";
/// The system-side interface handed to the service by `init`.
pub const SERVICE_CONNECTION: &str =
    "android.accessibilityservice.IAccessibilityServiceConnection";
/// `ActivityThread.PROCESS_STATE_TOP` (the state AMS reports for a bind it is
/// about to deliver).
const PROCESS_STATE_TOP: i32 = 2;
/// `Intent.FLAG_ACTIVITY_NEW_TASK` is not set for a service bind; AOSP's own
/// bind intent carries no flags.
const INTENT_FLAGS: i32 = 0;

/// The diagnostic the module reports when a bind cannot be planned.
fn rejected(detail: impl Into<String>) -> ServiceError {
    ServiceError::ModuleRejected {
        module: "accessibility",
        detail: detail.into(),
    }
}

/// The bind token for the `n`th system-initiated bind.
///
/// It rides through the app as an opaque `IBinder` and comes back in
/// `publishService`, which is how the reply is matched to the request. It is a
/// host node id, so the framework's `publishService` call returns the very same
/// number (`JAVA_TAG`/`NODE_TAG` round trip in `acos-binder-peer`).
#[must_use]
pub fn token_for(n: u32) -> u64 {
    0x1A11_0000_0000_0000 | u64::from(n)
}

/// The bind `Intent` for a component, in `Intent.writeToParcel` order.
#[must_use]
pub fn bind_intent(package: &str, class: &str) -> Parcelable {
    Parcelable::ordered(
        crate::parcelable::INTENT_CLASS,
        vec![
            Value::Str(Some(SERVICE_INTERFACE.to_string())), // mAction
            Value::Str(None),                                // mData (null Uri)
            Value::Str(None),                                // mType
            Value::Str(None),                                // mIdentifier
            Value::Int(INTENT_FLAGS),                        // mFlags
            Value::Str(None),                                // mPackage
            Value::Parcelable(Parcelable::ordered(
                "android.content.ComponentName",
                vec![
                    Value::Str(Some(package.to_string())),
                    Value::Str(Some(class.to_string())),
                ],
            )),
            Value::Parcelable(Parcelable::Null), // mSourceBounds
            Value::StringList(None),             // mCategories
            Value::Parcelable(Parcelable::Null), // mSelector
            Value::Parcelable(Parcelable::Null), // mClipData
            Value::Int(0),                       // mContentUserHint
            Value::Bundle(None),                 // mExtras
            Value::Parcelable(Parcelable::Null), // mOriginalIntent
        ],
    )
}

/// `CompatibilityInfo.DEFAULT_COMPATIBILITY_INFO`, in its `writeToParcel`
/// order (`compatibilityFlags`, `applicationDensity`, `applicationScale`,
/// `applicationInvertedScale`).
const DEFAULT_COMPAT: [u8; 16] = [0; 16];

fn default_compat_info() -> Parcelable {
    // `CompatibilityInfo` is four scalars; the default instance is all zero
    // except `applicationScale`/`applicationInvertedScale` = 1.0f.
    let _ = DEFAULT_COMPAT;
    Parcelable::ordered(
        "android.content.res.CompatibilityInfo",
        vec![
            Value::Int(0),     // compatibilityFlags (DEFAULT)
            Value::Int(160),   // applicationDensity (DisplayMetrics.DENSITY_DEFAULT)
            Value::Float(1.0), // applicationScale
            Value::Float(1.0), // applicationInvertedScale
        ],
    )
}

fn request(interface: &str, code: u32, args: Vec<Value>) -> Result<Parcel> {
    let method = acos_aidl::lookup(interface, code).ok_or_else(|| {
        ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code,
            method: format!("code {code}"),
        }
    })?;
    crate::codec::build_request(interface, method, &args)
}

/// The deliveries that create and bind one enabled accessibility service.
///
/// `state` supplies the app thread (from `attachApplication`) and the package
/// manager record (an APK-installed service, read lazily — the launch config
/// alone is not enough, ADR-0029 amendment).
pub fn plan_bind(state: &mut A11yState) -> Result<Vec<PendingDelivery>> {
    if !state.enabled || state.enabled_services.is_empty() {
        return Ok(Vec::new());
    }
    if state.bound_service.is_some() {
        return Ok(Vec::new());
    }
    let app_thread = state.app_thread.ok_or_else(|| {
        rejected("no IApplicationThread yet: attachApplication has not arrived, so the bind has no target")
    })?;
    let component = state.enabled_services[0].clone();
    let (package, class) = component
        .split_once('/')
        .ok_or_else(|| rejected(format!("{component:?} is not `package/.Class`")))?;
    let class = if let Some(short) = class.strip_prefix('.') {
        format!("{package}.{short}")
    } else {
        class.to_string()
    };
    let (service_info, app_info) = {
        let pms = state.pms.as_ref().ok_or_else(|| {
            rejected("no package manager: the installed service record cannot be read")
        })?;
        let pms = pms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let service = pms
            .services
            .iter()
            .find(|s| s.name == class && s.package_name == package)
            .cloned()
            .ok_or_else(|| {
                rejected(format!(
                    "the installed package {} declares no service {class}",
                    pms.app.package_name
                ))
            })?;
        (service, pms.app.clone())
    };
    if service_info.permission != "android.permission.BIND_ACCESSIBILITY_SERVICE" {
        return Err(rejected(format!(
            "service {component} is not guarded by android.permission.BIND_ACCESSIBILITY_SERVICE"
        )));
    }
    // `scheduleCreateService` carries the **component** `ServiceInfo`; the
    // `AccessibilityServiceInfo` the service later reads from
    // `getServiceInfo()` is built from the manifest's
    // `<accessibility-service>` meta-data (AOSP's
    // `AccessibilityManagerService` parses that block, not the component).
    let create_info = service_info.to_parcelable(&app_info);
    let acc_info = accessibility_info_for(&component, &service_info.meta_data);
    let token = token_for(state.next_bind_seq as u32 + 1);
    let intent = bind_intent(package, &class);
    // 1. `scheduleCreateService(token, ServiceInfo, CompatibilityInfo, int)`:
    //    `ActivityThread.handleBindService` only runs for a service that is in
    //    `mServices`, which `handleCreateService` fills.
    let create = request(
        APP_THREAD,
        3,
        vec![
            Value::Binder(Some(BinderId(token))),
            Value::Parcelable(create_info.clone()),
            Value::Parcelable(default_compat_info()),
            Value::Int(PROCESS_STATE_TOP),
        ],
    )?;
    // 2. `scheduleBindService(token, Intent, boolean rebind, int processState,
    //    long bindSeq)`. `rebind = false` is what makes the framework run
    //    `Service.onBind` and report the binder through `publishService`.
    let bind = request(
        APP_THREAD,
        11,
        vec![
            Value::Binder(Some(BinderId(token))),
            Value::Parcelable(intent.clone()),
            Value::Bool(false),
            Value::Int(PROCESS_STATE_TOP),
            Value::Long(state.next_bind_seq),
        ],
    )?;
    state.next_bind_seq += 1;
    state.pending_bind = Some(crate::misc::A11yPendingBind {
        component: component.clone(),
        package: package.to_string(),
        class,
        token,
        seq: state.next_bind_seq - 1,
        info: acc_info,
    });
    Ok(vec![
        PendingDelivery {
            target: BinderId(app_thread),
            interface: APP_THREAD.to_string(),
            code: 3,
            method: "scheduleCreateService".into(),
            data: create,
        },
        PendingDelivery {
            target: BinderId(app_thread),
            interface: APP_THREAD.to_string(),
            code: 11,
            method: "scheduleBindService".into(),
            data: bind,
        },
    ])
}

/// The `IAccessibilityServiceClient.init` delivery for a `publishService`
/// reply: `init(IAccessibilityServiceConnection connection, int connectionId,
/// IBinder windowToken)` (oneway, code 1).
///
/// The connection is the dynamic node registered by
/// [`crate::host::ServiceHost::install_accessibility`]; `windowToken` is the
/// display's overlay window token (ACOS has one display, and the token is only
/// ever handed back to `getOverlayWindowToken`, so it is this host's own
/// overlay token rather than a fabricated framework object).
#[must_use]
pub fn init_delivery(
    service_binder: u64,
    connection_binder: BinderId,
    connection_id: i32,
    window_token: BinderId,
) -> PendingDelivery {
    let mut data = Parcel::new();
    // Built by hand because `init`'s reply shape has no `acos-aidl` table (the
    // accessibility service-side AIDL is not in the decoded set); the argument
    // order is the interface's own and every value is a binder/int.
    data.write_interface_token(SERVICE_CLIENT)
        .expect("parcel write");
    data.write_strong_binder(connection_binder);
    data.write_int32(connection_id);
    data.write_strong_binder(window_token);
    PendingDelivery {
        target: BinderId(service_binder),
        interface: SERVICE_CLIENT.to_string(),
        code: 1,
        method: "init".into(),
        data,
    }
}

/// `IAccessibilityServiceConnection`: the object a bound accessibility service
/// talks to.
///
/// AOSP's implementation lives in `AccessibilityManagerService` and is backed by
/// the whole system; ACOS answers the transactions it can serve from the
/// session's own state (the service info it was bound with) and refuses the
/// rest with a structured `UNSUPPORTED_ANDROID_API`, never a made-up reply.
pub struct A11yConnectionNode {
    state: Arc<Mutex<A11yState>>,
}

impl A11yConnectionNode {
    /// Wrap the module state.
    #[must_use]
    pub fn new(state: Arc<Mutex<A11yState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for A11yConnectionNode {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[Value],
    ) -> Result<CallResult> {
        if interface != SERVICE_CONNECTION {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }

    /// `IAccessibilityServiceConnection` has no decoded `acos-aidl` table (the
    /// accessibility service-side AIDL is not in the image's decoded set), so
    /// the node answers raw transactions. The transaction codes are the order
    /// of the interface's own declarations
    /// (`base/core/java/android/accessibilityservice/IAccessibilityServiceConnection.aidl`,
    /// API 34), which is what the generated `Stub` numbers.
    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> Result<bool> {
        if interface != SERVICE_CONNECTION {
            return Ok(false);
        }
        let name = CONNECTION_METHODS
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, n)| *n);
        let Some(name) = name else {
            return Ok(false);
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match name {
            "setServiceInfo" => {
                let present = data.read_int32()?;
                if present != 0 {
                    let info = crate::parcelable::read_payload(
                        "android.accessibilityservice.AccessibilityServiceInfo",
                        data,
                    )?;
                    state.set_service_info(Some(Value::Parcelable(info)));
                }
                crate::exception::write_no_exception(reply);
            }
            "setAttributionTag" => {
                // `AccessibilityService` sets its attribution tag on the
                // connection right after `init`; AOSP records it and returns.
                let _tag = data.read_string16()?;
                crate::exception::write_no_exception(reply);
            }
            "getServiceInfo" => {
                let info = state
                    .bound_info()
                    .or_else(|| {
                        state
                            .enabled_services
                            .first()
                            .and_then(|c| service_info_value(c))
                    })
                    .unwrap_or(Value::Parcelable(Parcelable::Null));
                crate::exception::write_no_exception(reply);
                // `writeTypedObject`: present, then the payload.
                let _ = reply.write_int32(1);
                crate::parcelable::write_payload(
                    reply,
                    match &info {
                        Value::Parcelable(p) => p,
                        _ => &Parcelable::Null,
                    },
                )?;
            }
            "getSystemActions" => {
                // No registered system actions: the framework reads an empty
                // list, which is exactly what a device with none reports.
                crate::exception::write_no_exception(reply);
                let _ = reply.write_int32(0);
            }
            "disableSelf" => {
                state.self_disabled = true;
                crate::exception::write_no_exception(reply);
            }
            "performGlobalAction" => {
                let action = data.read_int32()?;
                state.unsupported.push(format!("performGlobalAction({action})"));
                return Err(ServiceError::ModuleRejected {
                    module: "accessibility",
                    detail: format!(
                        "performGlobalAction({action}) is not reachable from a bound service yet: the 
                         session's global actions are driven by the control plane (BACK) and need 
                         system apps for HOME/RECENTS/NOTIFICATIONS"
                    ),
                });
            }
            other => {
                state.unsupported.push(format!("{interface}.{other}"));
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// `IAccessibilityServiceConnection` transaction codes (AIDL declaration
/// order), for the methods this node answers.
const CONNECTION_METHODS: [(u32, &str); 6] = [
    (1, "setServiceInfo"),
    (2, "setAttributionTag"),
    (11, "getServiceInfo"),
    (12, "performGlobalAction"),
    (13, "getSystemActions"),
    (14, "disableSelf"),
];

/// `IAccessibilityServiceClient.init`'s connection id: one per bound service.
pub const FIRST_CONNECTION_ID: i32 = 1;

/// Record the `publishService` reply and return the delivery the bind owes
/// next.
///
/// `args` is the decoded `publishService(IBinder token, Intent intent, IBinder
/// service)` request. A token that does not match the pending bind is a
/// structured refusal: a stale or foreign `publishService` must not look like
/// our service connecting.
pub fn on_publish_service(
    state: &mut A11yState,
    connection_binder: BinderId,
    window_token: BinderId,
    args: &[Value],
) -> Result<Option<PendingDelivery>> {
    let token = args
        .first()
        .and_then(Value::as_binder)
        .map(|b| b.0)
        .ok_or_else(|| rejected("publishService without a binder token"))?;
    let Some(pending) = state.pending_bind.clone() else {
        return Err(rejected(format!(
            "publishService(token {token:#x}) arrived with no pending bind"
        )));
    };
    if pending.token != token {
        return Err(rejected(format!(
            "publishService token {token:#x} does not match the pending bind {:#x}",
            pending.token
        )));
    }
    let service = args
        .get(2)
        .and_then(Value::as_binder)
        .map(|b| b.0)
        .filter(|v| *v != 0)
        .ok_or_else(|| {
            rejected(format!(
                "the app published a null service binder for {}",
                pending.component
            ))
        })?;
    state.bind_service(
        &pending,
        service,
        FIRST_CONNECTION_ID,
        connection_binder.0,
        window_token.0,
    );
    state.pending_bind = None;
    Ok(Some(init_delivery(
        service,
        connection_binder,
        FIRST_CONNECTION_ID,
        window_token,
    )))
}

/// The `AccessibilityServiceInfo` a service is connected with.
///
/// AOSP builds it from the service's `<accessibility-service>` meta-data
/// (`AccessibilityServiceInfo.getDefaultInstance` + the parsed attributes). The
/// scalar attributes ACOS's manifest parser records are applied here; anything
/// the service declares that is not in the parsed set keeps the platform
/// default, which is what `service_info_value` writes.
#[must_use]
pub fn accessibility_info_for(
    component: &str,
    meta_data: &[MetaDataEntry],
) -> crate::value::Parcelable {
    let mut value = service_info_value(component)
        .unwrap_or(crate::value::Value::Parcelable(Parcelable::Null));
    if let crate::value::Value::Parcelable(Parcelable::Ordered { values, .. }) = &mut value {
        for entry in meta_data {
            let number = match &entry.value {
                crate::pms::MetaDataValue::Int(n) => Some(*n),
                crate::pms::MetaDataValue::String(text) => {
                    text.parse::<i32>().ok()
                }
                _ => None,
            };
            match entry.name.as_str() {
                // eventTypes (index 0), feedbackType (2), flags (6).
                "android:accessibilityEventTypes" => {
                    if let Some(n) = number {
                        values[0] = crate::value::Value::Int(n);
                    }
                }
                "android:accessibilityFeedbackType" => {
                    if let Some(n) = number {
                        values[2] = crate::value::Value::Int(n);
                    }
                }
                "android:accessibilityFlags" => {
                    if let Some(n) = number {
                        values[6] = crate::value::Value::Int(n);
                    }
                }
                "android:notificationTimeout" => {
                    if let Some(n) = number {
                        values[3] = crate::value::Value::Long(i64::from(n));
                    }
                }
                _ => {}
            }
        }
    }
    match value {
        crate::value::Value::Parcelable(p) => p,
        _ => Parcelable::Null,
    }
}
