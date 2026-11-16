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

/// `IAccessibilityInteractionConnection`: the app-side object the system calls
/// to reach a window's node tree.
pub const INTERACTION_CONNECTION: &str =
    "android.view.accessibility.IAccessibilityInteractionConnection";

/// `AccessibilityWindowInfo.UNDEFINED_WINDOW_ID`.
pub const UNDEFINED_WINDOW_ID: i32 = -1;

/// `AccessibilityWindowInfo.ACTIVE_WINDOW_ID`.
pub const ACTIVE_WINDOW_ID: i32 = 0x7fff_ffff;

/// `AccessibilityWindowInfo.WindowListSparseArray`'s item class, as
/// `Parcel.writeParcelable` names it.
const WINDOW_INFO_CLASS: &str = "android.view.accessibility.AccessibilityWindowInfo";

/// `AccessibilityWindowInfo.BOOLEAN_PROPERTY_*` masks.
const BOOLEAN_ACTIVE: i32 = 0x1;
const BOOLEAN_FOCUSED: i32 = 0x2;

/// Write one `AccessibilityWindowInfo` payload (no `writeTypedObject` presence
/// marker), in the image's `writeToParcel` order.
///
/// Transcribed from the API-34 `framework.jar` (`AccessibilityWindowInfo.
/// writeToParcel`: displayId, type, layer, booleanProperties, id, parentId,
/// taskId, Region, CharSequence title, anchorId, transitionTime, child ids,
/// connectionId, LocaleList). `Region.writeToParcel` is
/// `AParcel_writeVector<int32_t>`: a count and then `left/top/right/bottom`
/// per rectangle.
pub fn write_window_info(
    parcel: &mut Parcel,
    window: &crate::misc::A11yWindow,
    connection_id: i32,
) -> Result<()> {
    parcel.write_int32(0); // mDisplayId
    parcel.write_int32(window.type_);
    parcel.write_int32(window.layer);
    let mut properties = 0;
    if window.active {
        properties |= BOOLEAN_ACTIVE;
    }
    if window.focused {
        properties |= BOOLEAN_FOCUSED;
    }
    parcel.write_int32(properties);
    parcel.write_int32(window.id);
    parcel.write_int32(UNDEFINED_WINDOW_ID); // mParentId
    parcel.write_int32(UNDEFINED_WINDOW_ID); // mTaskId
    // `Region.writeToParcel`: one rectangle, as a vector of four ints.
    let (left, top, right, bottom) = window.frame;
    parcel.write_int32(4);
    parcel.write_int32(left);
    parcel.write_int32(top);
    parcel.write_int32(right);
    parcel.write_int32(bottom);
    // `writeCharSequence(null)`: no span marker, then a null string.
    parcel.write_int32(0);
    parcel.write_int32(-1);
    parcel.write_int64(0); // mAnchorId
    parcel.write_int64(0); // mTransitionTime
    parcel.write_int32(0); // child count
    parcel.write_int32(if window.connection.is_some() {
        connection_id
    } else {
        -1
    }); // mConnectionId
    // `writeParcelable(null)`: a null class name.
    parcel.write_int32(-1);
    Ok(())
}

/// Write the `AccessibilityWindowInfo.WindowListSparseArray` payload
/// (one display, its windows topmost first).
fn write_window_list(
    parcel: &mut Parcel,
    windows: &[crate::misc::A11yWindow],
    connection_id: i32,
) -> Result<()> {
    parcel.write_int32(1); // one display
    parcel.write_int32(i32::try_from(windows.len()).unwrap_or(i32::MAX));
    for window in windows {
        // `writeParcelable` writes the item's class name first.
        parcel.write_string16(WINDOW_INFO_CLASS)?;
        write_window_info(parcel, window, connection_id)?;
    }
    parcel.write_int32(0); // displayId
    Ok(())
}

/// One relayed interaction, as the service asked for it.
struct Interaction {
    /// `IAccessibilityInteractionConnection` transaction code.
    code: u32,
    /// `accessibilityNodeId` (the app resolves `0` to the window root).
    node_id: i64,
    /// The text / view id of a `findAccessibilityNodeInfosBy*` request.
    needle: Option<String>,
    /// `performAccessibilityAction`'s action id.
    action: i32,
    /// The `interactionId` the app must echo in the callback.
    interaction_id: i32,
    /// The service's `IAccessibilityInteractionConnectionCallback` binder.
    callback: u64,
    /// The service's fetch flags.
    flags: i32,
    /// The interrogating thread id the service reported.
    thread_id: i64,
    /// Byte range of a non-null `Bundle arguments` in the request parcel.
    bundle: Option<(usize, usize)>,
}

impl Interaction {
    /// Build the `IAccessibilityInteractionConnection` request parcel.
    ///
    /// The interface has no `acos-aidl` table (the app-side accessibility AIDL
    /// is not in the decoded set), so the argument order is the interface's own
    /// (`base/core/java/android/view/accessibility/IAccessibilityInteractionConnection.aidl`,
    /// API 34) and every value is a scalar, a binder or a typed object.
    fn request(&self, source: Option<&Parcel>) -> Result<Parcel> {
        let mut data = Parcel::new();
        data.write_interface_token(INTERACTION_CONNECTION)?;
        data.write_int64(self.node_id);
        if self.code == 6 {
            // performAccessibilityAction(long nodeId, int action, Bundle, int
            // interactionId, callback, int flags, int pid, long tid)
            data.write_int32(self.action);
            self.write_bundle(&mut data, source)?;
            data.write_int32(self.interaction_id);
            data.write_strong_binder(BinderId(self.callback));
            data.write_int32(self.flags);
            // `interrogatingPid`: the module does not model the guest pid, so 0
            // (unknown) is written rather than a made-up value.
            data.write_int32(0);
            data.write_int64(self.thread_id);
            return Ok(data);
        }
        // findAccessibilityNodeInfoByAccessibilityId(long, Region, int,
        // callback, int, int pid, long tid, MagnificationSpec, float[], Bundle)
        // and the text/view-id twins (long, String, Region, int, callback, int,
        // int, long, spec, float[]).
        match self.code {
            1 => {}
            2 | 3 => data.write_string16(self.needle.as_deref().unwrap_or(""))?,
            other => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: INTERACTION_CONNECTION.into(),
                    code: other,
                    method: format!("code {other}"),
                })
            }
        }
        // Region bounds: null (`writeTypedObject(null)`).
        data.write_int32(0);
        data.write_int32(self.interaction_id);
        data.write_strong_binder(BinderId(self.callback));
        data.write_int32(self.flags);
        data.write_int32(0);
        data.write_int64(self.thread_id);
        // MagnificationSpec: null.
        data.write_int32(0);
        // float[] matrixValues: null.
        data.write_int32(-1);
        if self.code == 1 {
            self.write_bundle(&mut data, source)?;
        }
        Ok(data)
    }

    /// Write `Bundle arguments`: the service's own payload bytes, or null.
    fn write_bundle(&self, data: &mut Parcel, source: Option<&Parcel>) -> Result<()> {
        match (self.bundle, source) {
            (Some((offset, len)), Some(source)) => {
                data.write_int32(1);
                data.append_from(source, offset, len)?;
            }
            _ => data.write_int32(0),
        }
        Ok(())
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
    deliveries: Vec<PendingDelivery>,
}

impl A11yConnectionNode {
    /// Wrap the module state.
    #[must_use]
    pub fn new(state: Arc<Mutex<A11yState>>) -> Self {
        Self {
            state,
            deliveries: Vec::new(),
        }
    }

    /// Relay one `findAccessibilityNodeInfo*` request to the window's
    /// `IAccessibilityInteractionConnection` and answer the service the way
    /// AOSP's `AccessibilityServiceConnection` does.
    ///
    /// AOSP's flow is asynchronous in the app but synchronous for the service:
    /// the system calls the connection (oneway) and returns the window's
    /// package names; the app then invokes the `callback` the service passed,
    /// and `AccessibilityInteractionClient` wakes the service thread waiting on
    /// that `interactionId`. ACOS keeps exactly that shape: the reply is
    /// written **now** (so the peer can send the oneway delivery in this turn),
    /// the request is queued against the app's own binder, and the callback
    /// comes back inside the guest (both binders are local there, so the result
    /// needs no host round trip).
    fn relay_find(
        &mut self,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
        state: &mut A11yState,
    ) -> Result<()> {
        // `IAccessibilityServiceConnection` argument order:
        //   int windowId, long nodeId, [String text|viewId], int interactionId,
        //   IBinder callback, int flags, long threadId[, Bundle arguments]
        let requested = data.read_int32()?;
        let node_id = data.read_int64()?;
        let needle = match code {
            4 | 5 => Some(data.read_string16()?.unwrap_or_default()),
            _ => None,
        };
        let interaction_id = data.read_int32()?;
        let callback = data
            .read_strong_binder()?
            .map(|b| b.0)
            .ok_or_else(|| ServiceError::BadRequest {
                interface: SERVICE_CONNECTION.into(),
                method: "findAccessibilityNodeInfo*".into(),
                detail: "missing IAccessibilityInteractionConnectionCallback".into(),
            })?;
        // Only `findAccessibilityNodeInfoByAccessibilityId` carries `flags`;
        // the text/view-id twins go straight from the callback to the thread id
        // (the image's `IAccessibilityServiceConnection` signatures).
        let flags = if code == 3 { data.read_int32()? } else { 0 };
        let thread_id = data.read_int64()?;
        let bundle = if code == 3 {
            let present = data.read_int32()?;
            (present != 0).then(|| (data.data_position(), data.remaining()))
        } else {
            None
        };
        let windows = state.window_snapshot();
        let resolved = state
            .resolve_window(&windows, requested)
            .and_then(|index| windows.get(index).cloned());
        let Some(window) = resolved else {
            // AOSP answers `null` when the window is gone / not visible to the
            // service; the client then returns null without waiting.
            crate::exception::write_no_exception(reply);
            let _ = reply.write_int32(-1);
            return Ok(());
        };
        let Some(connection) = window.connection else {
            // The window exists but the app has not registered an interaction
            // connection: nothing can answer, so the honest reply is "no
            // result" rather than a fabricated node.
            crate::exception::write_no_exception(reply);
            let _ = reply.write_int32(-1);
            return Ok(());
        };
        // Service code 3/4/5 -> interaction code 1/3/2.
        let interaction_code = match code {
            3 => 1,
            4 => 3,
            5 => 2,
            _ => unreachable!(),
        };
        let interaction = Interaction {
            code: interaction_code,
            node_id,
            needle,
            action: 0,
            interaction_id,
            callback,
            flags,
            thread_id,
            bundle,
        };
        let request = interaction.request(Some(data))?;
        self.deliveries.push(PendingDelivery {
            target: BinderId(connection),
            interface: INTERACTION_CONNECTION.to_string(),
            code: interaction_code,
            method: "findAccessibilityNodeInfo*".into(),
            data: request,
        });
        state.record_interaction(crate::misc::A11yInteraction {
            code,
            window: window.id,
            interaction_id,
            window_binder: window.window,
        });
        crate::exception::write_no_exception(reply);
        // AOSP returns the window's reported package names; the client treats
        // `null` as "no request went out" and a non-null array as "wait for the
        // callback".
        reply.write_string16_array(Some(&[window.package.as_str()]))?;
        Ok(())
    }


    /// Relay one `performAccessibilityAction` request to the window's
    /// `IAccessibilityInteractionConnection` (code 6).
    ///
    /// AOSP answers the service with whether the request went out; the real
    /// outcome arrives on the same callback (`setPerformAccessibilityActionResult`),
    /// which is dispatched inside the guest because both binders are local
    /// there.
    fn relay_action(
        &mut self,
        data: &mut Parcel,
        reply: &mut Parcel,
        state: &mut A11yState,
    ) -> Result<()> {
        // int windowId, long nodeId, int action, Bundle arguments,
        // int interactionId, callback, long threadId.
        let requested = data.read_int32()?;
        let node_id = data.read_int64()?;
        let action = data.read_int32()?;
        let present = data.read_int32()?;
        let bundle = (present != 0).then(|| (data.data_position(), data.remaining()));
        let interaction_id = data.read_int32()?;
        let callback = data
            .read_strong_binder()?
            .map(|b| b.0)
            .ok_or_else(|| ServiceError::BadRequest {
                interface: SERVICE_CONNECTION.into(),
                method: "performAccessibilityAction".into(),
                detail: "missing IAccessibilityInteractionConnectionCallback".into(),
            })?;
        let thread_id = data.read_int64()?;
        let windows = state.window_snapshot();
        let resolved = state
            .resolve_window(&windows, requested)
            .and_then(|index| windows.get(index).cloned())
            .and_then(|window| window.connection.map(|connection| (window, connection)));
        let Some((window, connection)) = resolved else {
            // AOSP returns `false` when the window or its connection is gone.
            crate::exception::write_no_exception(reply);
            reply.write_int32(0);
            return Ok(());
        };
        let interaction = Interaction {
            code: 6,
            node_id,
            needle: None,
            action,
            interaction_id,
            callback,
            flags: 0,
            thread_id,
            bundle,
        };
        let request = interaction.request(Some(data))?;
        self.deliveries.push(PendingDelivery {
            target: BinderId(connection),
            interface: INTERACTION_CONNECTION.to_string(),
            code: 6,
            method: "performAccessibilityAction".into(),
            data: request,
        });
        state.record_interaction(crate::misc::A11yInteraction {
            code: 8,
            window: window.id,
            interaction_id,
            window_binder: window.window,
        });
        crate::exception::write_no_exception(reply);
        reply.write_int32(1);
        Ok(())
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
        let shared = Arc::clone(&self.state);
        let mut state = shared
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
                // `AccessibilityService.GLOBAL_ACTION_BACK` is 1. AOSP dispatches
                // it as a Back **key** into the focused window, so the app's own
                // `Window` decides what it means: a dialog dismisses first, the
                // activity finishes after. ACOS routes it through exactly that
                // path — the session's `InputChannel` producer, the same one a
                // scenario `back` / `acos key back` uses — instead of calling
                // into the activity manager behind the app's back.
                // HOME/RECENTS/NOTIFICATIONS stay structured refusals: they are
                // system-UI actions and there is no system app to receive them.
                if action == GLOBAL_ACTION_BACK {
                    let token = state.back_token()?;
                    // Publish first: a failure here must be a structured error,
                    // not a `true` over a key that never left the host.
                    state.publish_back(token)?;
                    crate::exception::write_no_exception(reply);
                    // The boolean AOSP answers: the action was dispatched. The
                    // real outcome is the app's (it may consume Back without
                    // visible change), never something the module invents.
                    reply.write_int32(1);
                    return Ok(true);
                }
                state.unsupported.push(format!("performGlobalAction({action})"));
                return Err(ServiceError::ModuleRejected {
                    module: "accessibility",
                    detail: format!(
                        "performGlobalAction({action}) is not reachable from a bound service yet: 
                         BACK is routed to the session's Back path; HOME/RECENTS/NOTIFICATIONS 
                         are system-UI actions that need system apps"
                    ),
                });
            }
            "findAccessibilityNodeInfoByAccessibilityId"
            | "findAccessibilityNodeInfosByText"
            | "findAccessibilityNodeInfosByViewId" => {
                self.relay_find(code, data, reply, &mut state)?;
            }
            "performAccessibilityAction" => {
                self.relay_action(data, reply, &mut state)?;
            }
            "getWindows" => {
                crate::trace::line("ACOSA11Y getWindows enter");
                let windows = state.window_snapshot();
                crate::trace::line(format!("ACOSA11Y getWindows snapshot={}", windows.len()));
                let connection_id = state
                    .bound_service
                    .as_ref()
                    .map_or(-1, |b| b.connection_id);
                crate::exception::write_no_exception(reply);
                // `writeTypedObject(WindowListSparseArray)`: present, payload.
                let _ = reply.write_int32(1);
                write_window_list(reply, &windows, connection_id)?;
            }
            "getWindow" => {
                let requested = data.read_int32()?;
                let windows = state.window_list_readonly();
                let window = state
                    .resolve_window(&windows, requested)
                    .and_then(|index| windows.get(index).cloned());
                let connection_id = state
                    .bound_service
                    .as_ref()
                    .map_or(-1, |b| b.connection_id);
                crate::exception::write_no_exception(reply);
                match window {
                    Some(window) => {
                        let _ = reply.write_int32(1);
                        write_window_info(reply, &window, connection_id)?;
                    }
                    None => {
                        let _ = reply.write_int32(0);
                    }
                }
            }
            other => {
                state.unsupported.push(format!("{interface}.{other}"));
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn drain_raw_deliveries(&mut self) -> Vec<PendingDelivery> {
        std::mem::take(&mut self.deliveries)
    }
}

/// `IAccessibilityServiceConnection` transaction codes (AIDL declaration
/// order), for the methods this node answers.
const CONNECTION_METHODS: [(u32, &str); 12] = [
    (1, "setServiceInfo"),
    (2, "setAttributionTag"),
    (3, "findAccessibilityNodeInfoByAccessibilityId"),
    (4, "findAccessibilityNodeInfosByText"),
    (5, "findAccessibilityNodeInfosByViewId"),
    (8, "performAccessibilityAction"),
    (9, "getWindow"),
    (10, "getWindows"),
    (11, "getServiceInfo"),
    (12, "performGlobalAction"),
    (13, "getSystemActions"),
    (14, "disableSelf"),
];

/// `AccessibilityService.GLOBAL_ACTION_BACK` (the only global action the
/// module can route: a real Back key into the focused window).
pub const GLOBAL_ACTION_BACK: i32 = 1;

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
    // The overlay window token the service just received in `init` is the one
    // authority for `TYPE_ACCESSIBILITY_OVERLAY` windows: WMS accepts the add
    // only with it (`WindowManagerService.java:1630-1640`).
    state.register_overlay_token(window_token.0);
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
