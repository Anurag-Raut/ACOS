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
pub const SERVICE_CONNECTION: &str = "android.accessibilityservice.IAccessibilityServiceConnection";
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

/// Copy the parsed `<accessibility-service>` block into the
/// `AccessibilityServiceInfo` the bind hands the app.
///
/// Indices are [`crate::parcelable`]'s `AccessibilityServiceInfo` spec order:
/// 0 `eventTypes`, 2 `feedbackType`, 3 `notificationTimeout`, 6 `flags`,
/// 11 `mCapabilities`.
fn apply_service_config(
    info: &mut crate::value::Parcelable,
    config: crate::a11y_config::A11yServiceConfig,
) {
    let crate::value::Parcelable::Ordered { values, .. } = info else {
        return;
    };
    if values.len() < 12 {
        return;
    }
    values[0] = crate::value::Value::Int(config.event_types);
    values[2] = crate::value::Value::Int(config.feedback_type);
    values[3] = crate::value::Value::Long(config.notification_timeout);
    values[6] = crate::value::Value::Int(config.flags);
    values[11] = crate::value::Value::Int(config.capabilities);
}

/// Read the service's `<accessibility-service>` meta-data block from the
/// session's APK.
///
/// `AccessibilityServiceInfo.parseServiceInfo` reads the block the
/// `<meta-data android:name="android.accessibilityservice"
/// android:resource="@xml/…"/>` points at, and that is the only source of a
/// service's capabilities. A missing `android.accessibilityservice` entry, a
/// value that is not a resource, an unresolvable reference or a malformed
/// document leaves the service with **no** capabilities (reported through the
/// module's `unsupported` list), never with invented ones.
fn service_config_from_apk(
    state: &mut A11yState,
    meta_data: &[crate::pms::MetaDataEntry],
) -> Option<crate::a11y_config::A11yServiceConfig> {
    let resource_id = meta_data.iter().find_map(|entry| match entry.value {
        crate::pms::MetaDataValue::Reference(id)
            if entry.name == crate::a11y_config::SERVICE_META_DATA =>
        {
            Some(id)
        }
        _ => None,
    })?;
    let apk_path = match state.pms.as_ref() {
        Some(pms) => pms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .primary()
            .apk_path
            .clone(),
        None => {
            state
                .unsupported
                .push("accessibility meta-data: no package manager".into());
            return None;
        }
    };
    let apk = match crate::apk_facts::open_shared(&apk_path) {
        Ok(apk) => apk,
        Err(error) => {
            state.unsupported.push(format!(
                "accessibility meta-data: open {}: {error}",
                apk_path.display()
            ));
            return None;
        }
    };
    match crate::a11y_config::A11yServiceConfig::from_apk(&apk, resource_id) {
        Ok(config) => Some(config),
        Err(error) => {
            state.unsupported.push(format!(
                "accessibility meta-data @{resource_id:#010x}: {error}"
            ));
            None
        }
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

pub(crate) fn default_compat_info() -> Parcelable {
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
    let method =
        acos_aidl::lookup(interface, code).ok_or_else(|| ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code,
            method: format!("code {code}"),
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
        // A11Y2/MERGEMPROC: the service may live in **any** installed package
        // (the launched one or an extra APK), so the record is looked up across
        // the session's package table.
        let (service, app) = pms
            .packages
            .iter()
            .find_map(|installed| {
                installed
                    .services
                    .iter()
                    .find(|s| s.name == class && s.package_name == package)
                    .cloned()
                    .map(|service| (service, installed.app.clone()))
            })
            .ok_or_else(|| {
                rejected(format!(
                    "no installed package declares the service {component}"
                ))
            })?;
        (service, app)
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
    let mut acc_info = accessibility_info_for(&component, &service_info.meta_data);
    // MODA11Y6 step 2: the `<accessibility-service>` meta-data block is what
    // AOSP's `AccessibilityServiceInfo.parseServiceInfo` inflates: the resource
    // id in `<meta-data android:name="android.accessibilityservice"
    // android:resource="@xml/…"/>` -> the APK's resource table -> the zip entry
    // -> binary XML. A service that declares no block keeps capabilities 0,
    // which is exactly the answer `AccessibilitySecurityPolicy` gives it.
    let config = service_config_from_apk(state, &service_info.meta_data);
    if let Some(config) = config {
        // The `AccessibilityServiceInfo` the service reads back from
        // `getServiceInfo()` is the one parsed from that block in AOSP
        // (`AccessibilityServiceInfo.parseServiceInfo`), so the abilities the
        // host gates on and the abilities the app sees must be the same ones.
        apply_service_config(&mut acc_info, config);
    }
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
    state.service_config = config;
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

/// `IAccessibilityServiceClient.onAccessibilityEvent(in AccessibilityEvent,
/// in boolean serviceWantsEvent)` — the second oneway transaction of
/// `base/core/java/android/accessibilityservice/IAccessibilityServiceClient.aidl`.
pub const SERVICE_CLIENT_ON_ACCESSIBILITY_EVENT: u32 = 2;

/// `AccessibilityEvent.TYPE_WINDOWS_CHANGED` (`1 << 22`).
pub const TYPE_WINDOWS_CHANGED: i32 = 1 << 22;

/// `AccessibilityEvent.WINDOWS_CHANGE_ADDED`.
pub const WINDOWS_CHANGE_ADDED: i32 = 1;

/// `AccessibilityEvent.WINDOWS_CHANGE_REMOVED`.
pub const WINDOWS_CHANGE_REMOVED: i32 = 1 << 1;

/// `AccessibilityRecord.PROPERTY_IMPORTANT_FOR_ACCESSIBILITY` (`1 << 9`),
/// which `obtainWindowsChangedEvent` sets through
/// `setImportantForAccessibility(true)`.
const PROPERTY_IMPORTANT_FOR_ACCESSIBILITY: i32 = 1 << 9;

/// `AccessibilityRecord`'s `UNDEFINED` (`-1`).
const RECORD_UNDEFINED: i32 = -1;

/// `AccessibilityNodeInfo.UNDEFINED_NODE_ID`:
/// `makeNodeId(UNDEFINED_ITEM_ID, UNDEFINED_ITEM_ID)` with
/// `UNDEFINED_ITEM_ID == Integer.MAX_VALUE`.
const UNDEFINED_NODE_ID: i64 = ((i32::MAX as i64) << 32) | i32::MAX as i64;

/// One `IAccessibilityServiceClient.onAccessibilityEvent` delivery per queued
/// window-stack change, addressed at the bound service's client binder.
///
/// The service must hear about a window that appears *after* its first
/// `getWindows()`: `AccessibilityInteractionClient.getWindowsOnAllDisplays`
/// answers from the guest-side `AccessibilityCache` on a hit
/// (`AccessibilityInteractionClient.java:489-532`), and that cache is only
/// cleared by an accessibility event — `AccessibilityCache
/// .onAccessibilityEvent` clears it for `TYPE_WINDOWS_CHANGED`
/// (`AccessibilityCache.java:276-288`). AOSP sends exactly these events from
/// `AccessibilityWindowManager` (`obtainWindowsChangedEvent`, one per added,
/// removed or changed window), and `notifyAccessibilityEvent` delivers them
/// even when the service's `eventTypes` do not list them, because
/// `TYPE_WINDOWS_CHANGED` is cache-critical.
///
/// Nothing is delivered when no service is bound: an unbound service cannot
/// observe windows anyway, and a fabricated target would be a fake.
pub fn windows_changed_deliveries(state: &mut crate::misc::A11yState) -> Vec<PendingDelivery> {
    let changes = state.take_window_changes();
    if changes.is_empty() {
        return Vec::new();
    }
    let Some(bound) = state.bound_service.clone() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for change in changes {
        // AOSP addresses the event at the accessibility window id, which is
        // assigned per `IWindow` on first use (never reused).
        let window_id = state.window_id_for(change.window);
        let data = match windows_changed_event(bound.connection_id, window_id, change.changes) {
            Ok(data) => data,
            Err(error) => {
                eprintln!("{SERVICE_CLIENT}.onAccessibilityEvent: {error}");
                continue;
            }
        };
        out.push(PendingDelivery {
            target: BinderId(bound.binder),
            interface: SERVICE_CLIENT.to_string(),
            code: SERVICE_CLIENT_ON_ACCESSIBILITY_EVENT,
            method: "onAccessibilityEvent".into(),
            data,
        });
    }
    out
}

/// The `onAccessibilityEvent` request parcel for one `TYPE_WINDOWS_CHANGED`
/// event, in the image's `AccessibilityEvent.writeToParcel` /
/// `writeAccessibilityRecordToParcel` order
/// (`base/core/java/android/view/accessibility/AccessibilityEvent.java:1657-1720`,
/// the Java implementation this image runs — the app->system direction this
/// repo already decodes reads `isSealed` as the first int, which is the same
/// method).
fn windows_changed_event(connection_id: i32, window_id: i32, changes: i32) -> Result<Parcel> {
    let mut data = Parcel::new();
    data.write_interface_token(SERVICE_CLIENT)?;
    // The AIDL `in AccessibilityEvent` argument is a `writeTypedObject`, so the
    // presence marker comes first and `IAccessibilityServiceClient$Stub`
    // unmarshals it with `Parcel.readTypedObject(..., AccessibilityEvent.CREATOR)`.
    data.write_int32(1);
    // `AccessibilityEvent.writeToParcel`.
    data.write_int32(1); // mSealed (AMS seals every delivered event)
    data.write_int32(TYPE_WINDOWS_CHANGED);
    data.write_int32(0); // mMovementGranularity
    data.write_int32(0); // mAction
    data.write_int32(0); // mContentChangeTypes
    data.write_int32(changes); // mWindowChangeTypes
    data.write_int32(0); // mSpeechStateChangeTypes
                         // `TextUtils.writeToParcel`: a non-`Spanned` CharSequence is `int 1` and
                         // then `Parcel.writeString8` (the image's own
                         // `TextUtils.writeToParcel` at framework/classes3.dex 0x455fda-0x455ff6:
                         // `writeInt(1)`, `writeString8(cs == null ? null : cs.toString())`), so a
                         // null CharSequence is `1` + a `-1` length — not `0`, which is the
                         // `Spanned` branch and desynchronises the whole record.
    data.write_int32(1); // mPackageName: not spanned
    data.write_string8_opt(None)?; // mPackageName: null
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(0))
        .unwrap_or(0);
    data.write_int64(now_ms); // mEventTime
                              // AMS sets the record's connection id when the service may retrieve
                              // window content, and lets the event carry no source otherwise
                              // (`AbstractAccessibilityServiceConnection.notifyAccessibilityEvent`).
    data.write_int32(connection_id);
    // `writeAccessibilityRecordToParcel` for the event itself.
    data.write_int32(PROPERTY_IMPORTANT_FOR_ACCESSIBILITY); // mBooleanProperties
    data.write_int32(RECORD_UNDEFINED); // mCurrentItemIndex
    data.write_int32(RECORD_UNDEFINED); // mItemCount
    data.write_int32(RECORD_UNDEFINED); // mFromIndex
    data.write_int32(RECORD_UNDEFINED); // mToIndex
    data.write_int32(0); // mScrollX
    data.write_int32(0); // mScrollY
    data.write_int32(RECORD_UNDEFINED); // mScrollDeltaX
    data.write_int32(RECORD_UNDEFINED); // mScrollDeltaY
    data.write_int32(0); // mMaxScrollX
    data.write_int32(0); // mMaxScrollY
    data.write_int32(RECORD_UNDEFINED); // mAddedCount
    data.write_int32(RECORD_UNDEFINED); // mRemovedCount
    data.write_int32(1); // mClassName: not spanned
    data.write_string8_opt(None)?; // mClassName: null
    data.write_int32(1); // mContentDescription: not spanned
    data.write_string8_opt(None)?; // mContentDescription: null
    data.write_int32(1); // mBeforeText: not spanned
    data.write_string8_opt(None)?; // mBeforeText: null
    data.write_string16_opt(None)?; // mParcelableData: writeParcelable(null)
    data.write_int32(0); // mText: an empty list (AccessibilityRecord's field)
    data.write_int32(window_id); // mSourceWindowId
    data.write_int64(UNDEFINED_NODE_ID); // mSourceNodeId
    data.write_int32(-1); // mSourceDisplayId (Display.INVALID_DISPLAY)
    data.write_int32(0); // record mSealed
    data.write_int32(0); // record count
                         // `in boolean serviceWantsEvent`: the service asked for window changes or
                         // not; a cache-critical event is delivered either way, and `true` is what
                         // AOSP passes when it does want it.
    data.write_int32(1);
    Ok(data)
}

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
        let callback =
            data.read_strong_binder()?
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
    /// Serve one `dispatchGesture` request.
    ///
    /// Everything the app observes is the framework's: the sequence number and
    /// the sampled `MotionEvent`s come from the guest, the events go out on the
    /// session's real `InputChannel`, and the answer is
    /// `IAccessibilityServiceClient.onPerformGestureResult(sequence, delivered)`
    /// (`code 11`) **after** the events were published — a `true` here means
    /// the dispatcher accepted every event, never that the app handled it.
    fn dispatch_gesture(
        &mut self,
        sequence: i32,
        steps: &[crate::a11y_native::GestureStep],
        display_id: i32,
        state: &mut A11yState,
    ) -> crate::misc::A11yGestureRecord {
        let denied = |reason: String| crate::misc::A11yGestureRecord {
            sequence,
            events: steps.len(),
            delivered: false,
            reason: Some(reason),
        };
        if !state.can_perform_gestures() {
            // AOSP: `if (mServiceInterface != null &&
            // mSecurityPolicy.canPerformGestures(this))` — no injection and no
            // callback for a service without the capability.
            return denied(format!(
                "dispatchGesture({sequence}): the service declares no canPerformGestures \
                 capability, so AOSP neither injects nor answers"
            ));
        }
        let Some(bound) = state.bound_service.clone() else {
            return denied(format!(
                "dispatchGesture({sequence}): no accessibility service is bound"
            ));
        };
        if display_id != 0 {
            // AOSP resolves the injector **per display**
            // (`getMotionEventInjectorForDisplayLocked`); ACOS composites one
            // display (id 0), so a gesture for another display has no injector
            // and AOSP's own answer is `onPerformGestureResult(sequence,
            // false)`.
            let delivery = gesture_result_delivery(&bound, sequence, false);
            self.deliveries.push(delivery);
            return denied(format!(
                "dispatchGesture({sequence}): no motion event injector for display {display_id}"
            ));
        }
        let token = match state.gesture_token() {
            Ok(token) => token,
            Err(error) => {
                let delivery = gesture_result_delivery(&bound, sequence, false);
                self.deliveries.push(delivery);
                return denied(format!("dispatchGesture({sequence}): {error}"));
            }
        };
        let events = crate::a11y_native::GestureStep::gesture_events(steps);
        // The stroke's clock starts now (the same `CLOCK_MONOTONIC` base a real
        // `InputDispatcher` stamps events with) and every event keeps the
        // guest's own offset from it, so a 200 ms swipe is a 200 ms event-time
        // span even though the host publishes it in one turn.
        let down_time_ns = crate::input_channel::monotonic_nanos();
        let channel_events = crate::a11y_native::channel_events(&events, down_time_ns, display_id);
        for event in &events {
            let point = event.points.first().copied().unwrap_or((0.0, 0.0));
            crate::trace::line(format!(
                "ACOSA11Y dispatchGesture seq={sequence} t={} action={} at=({:.1},{:.1})",
                event.time_ms, event.action, point.0, point.1
            ));
        }
        match state.publish_gesture(token, &channel_events) {
            Ok(()) => {
                self.deliveries
                    .push(gesture_result_delivery(&bound, sequence, true));
                crate::misc::A11yGestureRecord {
                    sequence,
                    // The record counts the `MotionEvent`s the app received
                    // (one per `GestureStep` point), which is what "the stroke
                    // reached the list" means; `steps.len()` is the slice's
                    // sampling, not the touch stream.
                    events: events.len(),
                    delivered: true,
                    reason: None,
                }
            }
            Err(reason) => {
                self.deliveries
                    .push(gesture_result_delivery(&bound, sequence, false));
                denied(format!(
                    "dispatchGesture({sequence}): the session input channel refused the \
                     events: {reason}"
                ))
            }
        }
    }

    /// Serve one `takeScreenshot` request.
    ///
    /// The order and the codes are AOSP's `AbstractAccessibilityServiceConnection
    /// .takeScreenshot`: 333 ms interval (`ERROR_TAKE_SCREENSHOT_INTERVAL_TIME_SHORT`
    /// = 3) first, then the capability (the only failure that **throws**
    /// `SecurityException`), then the display (`…INVALID_DISPLAY` = 4), then a
    /// secure window (`…SECURE_WINDOW` = 6). Success is the
    /// `screenshot_*` `Bundle` through `IRemoteCallback.sendResult` with a
    /// `HardwareBuffer` over a memfd copy of the composited frame.
    fn take_screenshot(
        &mut self,
        display_id: i32,
        callback: Option<BinderId>,
        state: &mut A11yState,
        reply: &mut Parcel,
    ) -> Result<()> {
        /// `ACCESSIBILITY_TAKE_SCREENSHOT_REQUEST_INTERVAL_TIMES_MS`.
        const INTERVAL_MS: i64 = 333;
        /// `ERROR_TAKE_SCREENSHOT_INTERVAL_TIME_SHORT`.
        const ERROR_INTERVAL_TIME_SHORT: i32 = 3;
        /// `ERROR_TAKE_SCREENSHOT_INVALID_DISPLAY`.
        const ERROR_INVALID_DISPLAY: i32 = 4;
        /// `ERROR_TAKE_SCREENSHOT_SECURE_WINDOW`.
        const ERROR_SECURE_WINDOW: i32 = 6;
        let Some(callback) = callback else {
            return Err(crate::error::ServiceError::BadRequest {
                interface: SERVICE_CONNECTION.into(),
                method: "takeScreenshot".into(),
                detail: "a null RemoteCallback".into(),
            });
        };
        let now = A11yState::uptime_ms();
        let too_soon = state
            .last_screenshot_ms
            .is_some_and(|last| now.saturating_sub(last) <= INTERVAL_MS);
        if too_soon {
            self.screenshot_failure(callback, ERROR_INTERVAL_TIME_SHORT)?;
            Self::record_screenshot(state, display_id, ERROR_INTERVAL_TIME_SHORT);
            crate::exception::write_no_exception(reply);
            return Ok(());
        }
        state.last_screenshot_ms = Some(now);
        if !state.can_take_screenshot() {
            // AOSP throws: `throw new SecurityException("Services don't have
            // the capability of taking the screenshot.")`.
            state.unsupported.push(format!(
                "takeScreenshot({display_id}) denied: no capability"
            ));
            Self::record_screenshot(state, display_id, -1);
            crate::exception::write_exception(
                reply,
                crate::exception::EX_SECURITY,
                "Services don't have the capability of taking the screenshot.",
            )?;
            return Ok(());
        }
        // ACOS reports one display, the default one. AOSP refuses a display the
        // display manager does not know and a private virtual display; both are
        // "not this display" here.
        if display_id != 0 {
            self.screenshot_failure(callback, ERROR_INVALID_DISPLAY)?;
            Self::record_screenshot(state, display_id, ERROR_INVALID_DISPLAY);
            crate::exception::write_no_exception(reply);
            return Ok(());
        }
        if crate::a11y_native::secure_window_visible(state) {
            self.screenshot_failure(callback, ERROR_SECURE_WINDOW)?;
            Self::record_screenshot(state, display_id, ERROR_SECURE_WINDOW);
            crate::exception::write_no_exception(reply);
            return Ok(());
        }
        let frame = match crate::a11y_native::capture_frame(state) {
            Ok(frame) => frame,
            Err(status) => {
                self.screenshot_failure(callback, status)?;
                Self::record_screenshot(state, display_id, status);
                crate::exception::write_no_exception(reply);
                return Ok(());
            }
        };
        let mut data = Parcel::new();
        crate::a11y_native::write_success_bundle(&mut data, &frame, now)?;
        self.deliveries.push(PendingDelivery {
            target: BinderId(callback.0),
            interface: REMOTE_CALLBACK.to_string(),
            code: 1,
            method: "sendResult".into(),
            data,
        });
        Self::record_screenshot(
            state,
            display_id,
            crate::a11y_native::TAKE_SCREENSHOT_SUCCESS,
        );
        crate::trace::line(format!(
            "ACOSA11Y takeScreenshot success display={display_id} {}x{} stride={}",
            frame.width, frame.height, frame.stride
        ));
        crate::exception::write_no_exception(reply);
        Ok(())
    }

    /// Send one `sendScreenshotFailure` `Bundle` (`screenshot_status` only).
    fn screenshot_failure(&mut self, callback: BinderId, status: i32) -> Result<()> {
        let mut data = Parcel::new();
        crate::a11y_native::write_failure_bundle(&mut data, status)?;
        self.deliveries.push(PendingDelivery {
            target: callback,
            interface: REMOTE_CALLBACK.to_string(),
            code: 1,
            method: "sendResult".into(),
            data,
        });
        Ok(())
    }

    fn record_screenshot(state: &mut A11yState, display_id: i32, status: i32) {
        if state.screenshots.len() >= 32 {
            state.screenshots.remove(0);
        }
        state
            .screenshots
            .push(crate::misc::A11yScreenshotRecord { display_id, status });
    }

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
        let callback =
            data.read_strong_binder()?
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
    /// The service-side connection node drives the same `A11yState`: a
    /// `publishService` on it roots the bound service's client binder, and the
    /// turn that handled it is where the host learns about it.
    fn drain_retained(&mut self) -> Vec<BinderId> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .refs
            .drain_retained()
    }

    fn drain_released(&mut self) -> Vec<BinderId> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .refs
            .drain_released()
    }

    fn release_all_refs(&mut self) -> Vec<BinderId> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .refs
            .release_all()
    }

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
                state
                    .unsupported
                    .push(format!("performGlobalAction({action})"));
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
                // `canGetAccessibilityNodeInfoLocked` =
                // `canRetrieveWindowContentLocked && isRetrievalAllowingWindowLocked`;
                // ACOS reports every window in the LAYER1 stack, so the second
                // half (the window is known / active) always holds here and the
                // content capability is the gate. AOSP answers `null` — the
                // client sees "no node", not an exception.
                if !state.can_retrieve_window_content() {
                    state.unsupported.push(format!("{name} denied"));
                    crate::exception::write_no_exception(reply);
                    let _ = reply.write_int32(-1);
                    return Ok(true);
                }
                self.relay_find(code, data, reply, &mut state)?;
            }
            "performAccessibilityAction" => {
                // `performAccessibilityAction` gates on the same
                // `canGetAccessibilityNodeInfoLocked` and answers `false`.
                if !state.can_retrieve_window_content() {
                    state
                        .unsupported
                        .push("performAccessibilityAction denied".into());
                    crate::exception::write_no_exception(reply);
                    let _ = reply.write_int32(0);
                    return Ok(true);
                }
                self.relay_action(data, reply, &mut state)?;
            }
            "getWindows" => {
                crate::trace::line("ACOSA11Y getWindows enter");
                // `AccessibilitySecurityPolicy.canRetrieveWindowsLocked`
                // (`AbstractAccessibilityServiceConnection.getWindows`): a
                // service without the content capability + interactive-windows
                // flag gets `null`, which the framework turns into an empty
                // window list — never an error and never the list.
                if !state.can_retrieve_windows() {
                    state.unsupported.push("getWindows denied".into());
                    crate::exception::write_no_exception(reply);
                    let _ = reply.write_int32(0);
                    return Ok(true);
                }
                let windows = state.window_snapshot();
                crate::trace::line(format!("ACOSA11Y getWindows snapshot={}", windows.len()));
                let connection_id = state.bound_service.as_ref().map_or(-1, |b| b.connection_id);
                crate::exception::write_no_exception(reply);
                // `writeTypedObject(WindowListSparseArray)`: present, payload.
                let _ = reply.write_int32(1);
                write_window_list(reply, &windows, connection_id)?;
            }
            "getWindow" => {
                // Same gate as `getWindows`: AOSP's `getWindow` checks
                // `canRetrieveWindowsLocked` too and answers `null`.
                if !state.can_retrieve_windows() {
                    state.unsupported.push("getWindow denied".into());
                    crate::exception::write_no_exception(reply);
                    let _ = reply.write_int32(0);
                    return Ok(true);
                }
                let requested = data.read_int32()?;
                let windows = state.window_list_readonly();
                let window = state
                    .resolve_window(&windows, requested)
                    .and_then(|index| windows.get(index).cloned());
                let connection_id = state.bound_service.as_ref().map_or(-1, |b| b.connection_id);
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
            // `IAccessibilityServiceConnection.dispatchGesture(int sequence,
            // in ParceledListSlice gestureSteps, int displayId)` — code 33.
            //
            // The app's own `AccessibilityService.dispatchGesture` sampled the
            // strokes into `MotionEvent`s
            // (`GestureDescription$MotionEventGenerator`) and put them in the
            // slice, so the host receives real events and publishes them on the
            // session's input channel. AOSP's `AccessibilityServiceConnection`
            // serves the request only when
            // `mSecurityPolicy.canPerformGestures(this)` holds; without the
            // capability it neither injects nor answers, so neither does this
            // module — it records the refusal instead of inventing a callback.
            "dispatchGesture" => {
                crate::trace::line("ACOSA11Y dispatchGesture enter");
                let sequence = data.read_int32()?;
                // AIDL writes every `in Parcelable` argument with
                // `writeTypedObject`, i.e. its class name first and the payload
                // after it (a null is a `-1` length and no payload).
                expect_present(data, "dispatchGesture", "ParceledListSlice")?;
                let events = crate::a11y_native::read_gesture_slice(data)?;
                let display_id = data.read_int32()?;
                crate::trace::line(format!(
                    "ACOSA11Y dispatchGesture decoded seq={sequence} events={} display={display_id}",
                    events.len()
                ));
                let record = self.dispatch_gesture(sequence, &events, display_id, &mut state);
                if let Some(reason) = record.reason.clone() {
                    state.unsupported.push(reason);
                }
                state.gestures.push(record);
                crate::exception::write_no_exception(reply);
            }
            // `IAccessibilityServiceConnection.takeScreenshot(int displayId,
            // in RemoteCallback callback)` — code 37.
            //
            // AOSP's order (`AbstractAccessibilityServiceConnection
            // .takeScreenshot`): the 333 ms interval is checked **first**, then
            // the capability, then the display, then a secure window; the
            // capability failure is the only one that throws, every other
            // failure is a `Bundle` with `screenshot_status` = the error code
            // sent through `IRemoteCallback.sendResult`.
            "takeScreenshot" => {
                crate::trace::line("ACOSA11Y takeScreenshot enter");
                let display_id = data.read_int32()?;
                expect_present(data, "takeScreenshot", "RemoteCallback")?;
                let callback = data.read_strong_binder()?;
                self.take_screenshot(display_id, callback, &mut state, reply)?;
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

/// Read the presence marker `Parcel.writeTypedObject` writes before a
/// parcelable argument's payload.
///
/// AIDL generates `writeTypedObject` for every `in`/`out` parcelable argument:
/// `1` then the payload, or `0` for null (no class name — that is
/// `writeParcelable`'s shape, and the captured `dispatchGesture` parcel in
/// `tests/a11ynative.rs` shows the presence word immediately followed by the
/// slice's item count). A null argument is a structured refusal: neither
/// `dispatchGesture` nor `takeScreenshot` has a meaning without it.
fn expect_present(data: &mut Parcel, method: &str, what: &str) -> Result<()> {
    if data.read_int32()? != 0 {
        return Ok(());
    }
    Err(crate::error::ServiceError::BadRequest {
        interface: SERVICE_CONNECTION.into(),
        method: method.into(),
        detail: format!("a null {what}"),
    })
}

/// `android.os.IRemoteCallback` — the binder `takeScreenshot` hands its result
/// to (`sendResult(in Bundle)` = code 1, oneway).
pub const REMOTE_CALLBACK: &str = "android.os.IRemoteCallback";

/// `IAccessibilityServiceClient.onPerformGestureResult(int sequence, boolean
/// completed)` — code 11 on the service client (MODA11Y7 `wire-map.txt`).
pub const SERVICE_CLIENT_ON_PERFORM_GESTURE_RESULT: u32 = 11;

/// The `onPerformGestureResult` delivery for a served gesture request.
#[must_use]
pub fn gesture_result_delivery(
    bound: &crate::misc::A11yBoundService,
    sequence: i32,
    completed: bool,
) -> PendingDelivery {
    let mut data = Parcel::new();
    data.write_interface_token(SERVICE_CLIENT)
        .expect("parcel write");
    data.write_int32(sequence);
    data.write_int32(i32::from(completed));
    PendingDelivery {
        target: BinderId(bound.binder),
        interface: SERVICE_CLIENT.to_string(),
        code: SERVICE_CLIENT_ON_PERFORM_GESTURE_RESULT,
        method: "onPerformGestureResult".into(),
        data,
    }
}

/// `IAccessibilityServiceConnection` transaction codes (AIDL declaration
/// order), for the methods this node answers.
const CONNECTION_METHODS: [(u32, &str); 14] = [
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
    // Android 12+ interaction requests. The numbering is the image's own
    // `IAccessibilityServiceConnection$Stub.getDefaultTransactionName` packed
    // switch, not the AIDL file order (MODA11Y7 `wire-map.txt`).
    (33, "dispatchGesture"),
    (37, "takeScreenshot"),
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
    let mut value =
        service_info_value(component).unwrap_or(crate::value::Value::Parcelable(Parcelable::Null));
    if let crate::value::Value::Parcelable(Parcelable::Ordered { values, .. }) = &mut value {
        for entry in meta_data {
            let number = match &entry.value {
                crate::pms::MetaDataValue::Int(n) => Some(*n),
                crate::pms::MetaDataValue::String(text) => text.parse::<i32>().ok(),
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

/// The deliveries that **unbind** the currently bound accessibility service.
///
/// AOSP's `AccessibilityManagerService.removeServiceLocked` →
/// `unbindService` → the app's `ActivityThread` runs `Service.onUnbind`, then
/// `scheduleStopService` destroys it (`AccessibilityService.onUnbind`/
/// `onDestroy`). ACOS's bind created the service with
/// `scheduleCreateService` + `scheduleBindService` (`plan_bind`), so the
/// inverse is `scheduleUnbindService` + `scheduleStopService` addressed at the
/// same token — exactly the pair the generic `bindService` path already uses
/// (`crate::service_bind`).
///
/// The service's overlay window token is released with it: AOSP removes an
/// accessibility window token when the service that owns it goes away, and
/// leaving the token behind would let a later, unrelated window inherit the
/// overlay authority of a service that is no longer enabled.
pub fn plan_unbind(state: &mut A11yState) -> Vec<PendingDelivery> {
    let bound = state.bound_service.take();
    let pending = state.pending_bind.take();
    let (component, token) = match (&bound, &pending) {
        (Some(bound), _) => (bound.component.clone(), bound.token),
        (None, Some(pending)) => (pending.component.clone(), pending.token),
        (None, None) => return Vec::new(),
    };
    if let Some(bound) = &bound {
        state.release_overlay_token(bound.window_token);
        if bound.binder != 0 {
            state.refs.release(acos_parcel::BinderId(bound.binder));
        }
    }
    state.service_config = None;
    let (package, class) = component
        .split_once('/')
        .map(|(package, class)| {
            (
                package.to_string(),
                if let Some(short) = class.strip_prefix('.') {
                    format!("{package}.{short}")
                } else {
                    class.to_string()
                },
            )
        })
        .unwrap_or_else(|| (component.clone(), component.clone()));
    let Some(app_thread) = state.app_thread else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Ok(data) = request(
        APP_THREAD,
        SCHEDULE_UNBIND_SERVICE,
        vec![
            Value::Binder(Some(BinderId(token))),
            Value::Parcelable(bind_intent(&package, &class)),
        ],
    ) {
        out.push(PendingDelivery {
            target: BinderId(app_thread),
            interface: APP_THREAD.to_string(),
            code: SCHEDULE_UNBIND_SERVICE,
            method: "scheduleUnbindService".into(),
            data,
        });
    }
    if let Ok(data) = request(
        APP_THREAD,
        SCHEDULE_STOP_SERVICE,
        vec![Value::Binder(Some(BinderId(token)))],
    ) {
        out.push(PendingDelivery {
            target: BinderId(app_thread),
            interface: APP_THREAD.to_string(),
            code: SCHEDULE_STOP_SERVICE,
            method: "scheduleStopService".into(),
            data,
        });
    }
    out
}

/// `IApplicationThread.scheduleUnbindService(IBinder, Intent)`.
pub const SCHEDULE_UNBIND_SERVICE: u32 = 12;
/// `IApplicationThread.scheduleStopService(IBinder)`.
pub const SCHEDULE_STOP_SERVICE: u32 = 4;

/// `AccessibilityManagerService.onUserStateChangedLocked` →
/// `readConfigurationForUserStateLocked` → `setEnabledServices` →
/// `bindServices`.
///
/// The system learns that the enabled-service set changed from its
/// `SettingsObserver`, never from an app call. ACOS's equivalent observer is a
/// **pull** at the session's delivery drain: the settings provider's `PUT_secure`
/// and the module's own launches write the same table, so re-reading it there
/// is exactly what the observer sees, without a callback channel between two
/// service objects that share only the `SettingsState` lock.
///
/// Returns the deliveries the reconciliation produced (a bind for a newly
/// enabled service, the unbind pair for a service that left the set).
pub fn sync_enabled_services(state: &mut A11yState) -> Vec<PendingDelivery> {
    let Some(settings) = state.settings.as_ref() else {
        // No settings provider: nothing can change the enable set behind the
        // module's back, and `enable_services` already keeps the state in step.
        return Vec::new();
    };
    let (enabled, services, generation, written) = {
        let settings = settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A settings table nobody has written has no enable set to read: the
        // module's own record is then the authority (a host built without the
        // module's launch config, a conformance kit). Once either key exists —
        // every real session writes both when a service is switched on — the
        // table is the authority, exactly as it is on a device.
        let written = settings.secure.contains_key("accessibility_enabled")
            || settings
                .secure
                .contains_key("enabled_accessibility_services");
        (
            settings
                .secure
                .get("accessibility_enabled")
                .is_some_and(|value| value == "1"),
            settings
                .secure
                .get("enabled_accessibility_services")
                .map(|raw| {
                    raw.split(':')
                        .filter(|entry| !entry.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            settings.generation(crate::provider::Namespace::Secure),
            written,
        )
    };
    if !written {
        return Vec::new();
    }
    // The observer runs when the `Settings.Secure` generation moves — whether
    // the write came from a guest `ContentResolver` or from the module's own
    // `enable_services` (which writes the same two keys and then keeps its own
    // record in step). Reading the table on every drain is what makes the two
    // paths converge on one reconciliation.
    if state.observed_secure_generation == Some(generation)
        && enabled == state.enabled
        && services == state.enabled_services
    {
        return Vec::new();
    }
    state.observed_secure_generation = Some(generation);
    state.enabled = enabled;
    state.enabled_services = services;
    // A service that is no longer enabled (or was replaced by another one) is
    // unbound first: `setEnabledServices` unbinds the services that left the
    // set before it binds the ones that joined it.
    let keep = state
        .enabled
        .then(|| state.enabled_services.first().cloned())
        .flatten();
    let bound = state
        .bound_service
        .as_ref()
        .map(|bound| bound.component.clone())
        .or_else(|| state.pending_bind.as_ref().map(|bind| bind.component.clone()));
    let mut deliveries = Vec::new();
    if let Some(bound) = bound {
        if keep.as_deref() != Some(bound.as_str()) {
            deliveries.extend(plan_unbind(state));
        }
    }
    if keep.is_some() {
        match plan_bind(state) {
            Ok(more) => deliveries.extend(more),
            Err(error) => {
                // A bind that cannot start yet (no `attachApplication`, no
                // installed service) is recorded on the state, never a silent
                // success: the module's report lists it.
                let reason = format!("bind not started: {error}");
                if !state.unsupported.contains(&reason) {
                    state.unsupported.push(reason);
                }
            }
        }
    }
    deliveries
}
