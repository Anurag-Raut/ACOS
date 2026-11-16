//! MODA11Y6 step 2: the `getWindows` capability gate.
//!
//! AOSP decides whether a bound service may see the window list from the
//! service's own `<accessibility-service>` meta-data block, not from the
//! manifest attributes:
//!
//! ```text
//! AccessibilitySecurityPolicy.canRetrieveWindowContentLocked
//!     = capabilities & CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT   (1)
//! AccessibilitySecurityPolicy.canRetrieveWindowsLocked
//!     = canRetrieveWindowContentLocked && mRetrieveInteractiveWindows
//!     (mRetrieveInteractiveWindows = flags & FLAG_RETRIEVE_INTERACTIVE_WINDOWS, 1 << 6)
//! AccessibilitySecurityPolicy.canGetAccessibilityNodeInfoLocked
//!     = canRetrieveWindowContentLocked && isRetrievalAllowingWindowLocked
//! ```
//!
//! (`AccessibilitySecurityPolicy.java:317-346`,
//! `AbstractAccessibilityServiceConnection.java:449-459`.)
//!
//! Every assertion below runs against the **real fixture APK**: its resource
//! table resolves `@xml/accessibility_service_config`, the zip entry is parsed
//! as binary XML, and the resulting capabilities drive the same handler the
//! guest's binder calls.

use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};
use acos_services::a11y_config::{
    A11yServiceConfig, CAPABILITY_CAN_PERFORM_GESTURES, CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT,
    CAPABILITY_CAN_TAKE_SCREENSHOT, FLAG_RETRIEVE_INTERACTIVE_WINDOWS, SERVICE_META_DATA,
};
use acos_services::host::ServiceHost;
use acos_services::misc::{A11yPendingBind, A11yService, A11yState};
use acos_services::pms::{MetaDataValue, PmsState};
use acos_services::value::Parcelable;

const MANAGER: &str = "android.view.accessibility.IAccessibilityManager";
const CONNECTION: &str = "android.accessibilityservice.IAccessibilityServiceConnection";
const PACKAGE: &str = "dev.acos.fixtures.a11yprobe";
const APK: &str = "fixtures/build/apps/a11y-probe.apk";

fn fixture_apk() -> std::path::PathBuf {
    acos_services::workspace_path(APK).unwrap_or_else(|| panic!("{APK} is built"))
}

fn installed_pms() -> PmsState {
    PmsState::from_apk(fixture_apk()).expect("the fixture APK parses")
}

/// The resource id the given service declares for `android.accessibilityservice`.
fn declared_resource(pms: &PmsState, service: &str) -> u32 {
    let record = pms
        .primary()
        .services
        .iter()
        .find(|s| s.name == service)
        .unwrap_or_else(|| panic!("{service} is declared"));
    record
        .meta_data
        .iter()
        .find_map(|entry| match entry.value {
            MetaDataValue::Reference(id) if entry.name == SERVICE_META_DATA => Some(id),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{service} declares the accessibility meta-data resource"))
}

fn config_for(service: &str) -> A11yServiceConfig {
    let pms = installed_pms();
    let id = declared_resource(&pms, service);
    let apk = acos_apk::Apk::open(fixture_apk()).expect("open");
    A11yServiceConfig::from_apk(&apk, id).expect("the meta-data block parses")
}

#[test]
fn the_probe_service_declares_window_content_and_interactive_windows() {
    let config = config_for("dev.acos.fixtures.a11yprobe.A11yProbeService");
    assert!(
        config.can_retrieve_window_content(),
        "android:canRetrieveWindowContent=true -> CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT"
    );
    assert_eq!(
        config.flags & FLAG_RETRIEVE_INTERACTIVE_WINDOWS,
        FLAG_RETRIEVE_INTERACTIVE_WINDOWS,
        "flagRetrieveInteractiveWindows is bit 6 (0x40), not 0x2"
    );
    assert!(config.can_retrieve_windows(), "the window list is allowed");
    assert!(
        config.can_perform_gestures(),
        "android:canPerformGestures -> CAPABILITY_CAN_PERFORM_GESTURES"
    );
    assert!(
        config.can_take_screenshot(),
        "android:canTakeScreenshot -> CAPABILITY_CAN_TAKE_SCREENSHOT"
    );
    assert_eq!(
        config.capabilities & CAPABILITY_CAN_PERFORM_GESTURES,
        CAPABILITY_CAN_PERFORM_GESTURES
    );
    assert_eq!(
        config.capabilities & CAPABILITY_CAN_TAKE_SCREENSHOT,
        CAPABILITY_CAN_TAKE_SCREENSHOT
    );
    assert_ne!(
        config.event_types, 0,
        "android:accessibilityEventTypes parsed"
    );
    assert_eq!(config.notification_timeout, 100);
}

#[test]
fn the_bare_service_declares_no_window_content_capability() {
    let config = config_for("dev.acos.fixtures.a11yprobe.A11yBareService");
    assert!(
        !config.can_retrieve_window_content(),
        "android:canRetrieveWindowContent=false"
    );
    assert!(
        !config.can_retrieve_windows(),
        "and therefore no window list"
    );
    assert!(!config.can_perform_gestures());
    assert!(!config.can_take_screenshot());
    assert_eq!(
        config.capabilities & CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT,
        0
    );
}

#[test]
fn a_service_with_no_meta_data_has_no_capabilities() {
    // AOSP's `AccessibilityServiceInfo` for a service with no block has
    // capabilities 0, so the whole interaction surface is refused.
    let empty = A11yServiceConfig::default();
    assert!(!empty.can_retrieve_window_content());
    assert!(!empty.can_retrieve_windows());
}

// ---------------------------------------------------------------------------
// The handlers, through the real host
// ---------------------------------------------------------------------------

fn a11y_state(host: &mut ServiceHost) -> Arc<Mutex<A11yState>> {
    host.ensure(MANAGER)
        .expect("A11yService")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<A11yService>())
        .expect("A11yService")
        .state()
}

fn java(wire: u32) -> u64 {
    1 << 32 | u64::from(wire)
}

/// Bind `client` with `config` as its declared meta-data (that is what
/// `plan_bind` stores from the APK) and add one application window.
fn host_with_bound(config: A11yServiceConfig) -> ServiceHost {
    let mut host = ServiceHost::standard();
    let state = a11y_state(&mut host);
    let mut state = state.lock().expect("a11y state");
    state.service_config = Some(config);
    state.bind_service(
        &A11yPendingBind {
            component: format!("{PACKAGE}/.A11yProbeService"),
            package: PACKAGE.to_string(),
            class: format!("{PACKAGE}.A11yProbeService"),
            token: 0x1A11_0000_0000_0001,
            seq: 0,
            info: Parcelable::Null,
        },
        java(0x41),
        1,
        0,
        0,
    );
    let wms = state.windows.clone().expect("the LAYER1 stack is wired");
    drop(state);
    let mut wms = wms.lock().expect("wms");
    let session = wms.session;
    wms.wm
        .add_to_display_as_user(
            session,
            BinderId(0x7001),
            acos_wm::codec::LayoutParams::application(PACKAGE, "MainActivity"),
            0,
        )
        .expect("window add");
    drop(wms);
    host
}

fn connection_node(host: &mut ServiceHost) -> BinderId {
    let state = a11y_state(host);
    let state = state.lock().expect("a11y state");
    let connection = state.bound_service.as_ref().expect("bound").connection;
    drop(state);
    if connection == 0 {
        host.accessibility_connection_node()
    } else {
        BinderId(connection)
    }
}

fn transact(host: &mut ServiceHost, code: u32, args: impl FnOnce(&mut Parcel)) -> Parcel {
    let node = connection_node(host);
    let mut data = Parcel::new();
    data.write_interface_token(CONNECTION).expect("token");
    args(&mut data);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(node, code, &mut data, &mut reply, 0)
        .expect("connection transaction");
    reply.set_data_position(0);
    reply
}

const FIND_BY_ACCESSIBILITY_ID: u32 = 3;
const PERFORM_ACTION: u32 = 8;
const GET_WINDOW: u32 = 9;
const GET_WINDOWS: u32 = 10;

#[test]
fn a_service_with_the_capability_gets_the_window_list() {
    let config = config_for("dev.acos.fixtures.a11yprobe.A11yProbeService");
    let mut host = host_with_bound(config);
    let mut reply = transact(&mut host, GET_WINDOWS, |_| {});
    assert_eq!(reply.read_int32().expect("exception"), 0);
    assert_eq!(reply.read_int32().expect("typed object present"), 1);
    assert_eq!(reply.read_int32().expect("one display"), 1);
    assert_eq!(reply.read_int32().expect("window count"), 1);
}

#[test]
fn a_service_without_the_capability_gets_aosp_null_not_an_exception() {
    let config = config_for("dev.acos.fixtures.a11yprobe.A11yBareService");
    assert!(!config.can_retrieve_windows());
    let mut host = host_with_bound(config);

    // getWindows: a null `WindowListSparseArray` (presence 0). The framework's
    // `AccessibilityInteractionClient.getWindowsOnAllDisplays` turns that into
    // an empty list — no exception reaches the service.
    let mut reply = transact(&mut host, GET_WINDOWS, |_| {});
    assert_eq!(reply.read_int32().expect("exception"), 0, "no exception");
    assert_eq!(reply.read_int32().expect("typed object present"), 0, "null");

    // getWindow: null as well.
    let mut reply = transact(&mut host, GET_WINDOW, |data| data.write_int32(1));
    assert_eq!(reply.read_int32().expect("exception"), 0);
    assert_eq!(reply.read_int32().expect("typed object present"), 0, "null");

    // find*: AOSP's `canGetAccessibilityNodeInfoLocked` refuses, the client
    // gets no node (ACOS answers the same "nothing will call you back" -1 the
    // relay uses for a window that cannot answer).
    let mut reply = transact(&mut host, FIND_BY_ACCESSIBILITY_ID, |data| {
        data.write_int32(0); // window id 0 = active
        data.write_int64(0); // node id
        data.write_int32(1); // interactionId
        data.write_strong_binder_opt(Some(BinderId(java(0x51)))); // callback
        data.write_int32(0); // flags
        data.write_int64(7); // thread id
        data.write_int32(0); // Bundle arguments: absent
    });
    assert_eq!(reply.read_int32().expect("exception"), 0);
    assert_eq!(reply.read_int32().expect("interaction"), -1, "no node");
    let state = a11y_state(&mut host);
    assert!(
        state
            .lock()
            .expect("a11y state")
            .unsupported
            .iter()
            .any(|u| u.starts_with("findAccessibilityNodeInfoByAccessibilityId denied")),
        "the node query is refused by the capability gate"
    );
    drop(state);

    // performAccessibilityAction: `false`.
    let mut reply = transact(&mut host, PERFORM_ACTION, |data| {
        data.write_int32(0); // window id
        data.write_int64(0); // node id
        data.write_int32(0x10); // ACTION_CLICK
        data.write_int32(0); // Bundle arguments: absent
        data.write_int32(1); // interactionId
        data.write_strong_binder_opt(Some(BinderId(java(0x52)))); // callback
        data.write_int64(7); // thread id
    });
    assert_eq!(reply.read_int32().expect("exception"), 0);
    assert_eq!(reply.read_int32().expect("performed"), 0, "false");

    // The refusals are recorded, never silent.
    let state = a11y_state(&mut host);
    let unsupported = state.lock().expect("a11y state").unsupported.clone();
    assert!(
        unsupported.iter().any(|u| u == "getWindows denied"),
        "the refusal is reported: {unsupported:?}"
    );
}

#[test]
fn the_content_capability_without_the_interactive_windows_flag_hides_the_list() {
    // `canRetrieveWindowContent` alone is enough for node queries but **not**
    // for the window list: AOSP's `canRetrieveWindowsLocked` also needs
    // `FLAG_RETRIEVE_INTERACTIVE_WINDOWS`
    // (`AccessibilitySecurityPolicy.java:330-333`).
    let mut config = config_for("dev.acos.fixtures.a11yprobe.A11yProbeService");
    config.flags &= !FLAG_RETRIEVE_INTERACTIVE_WINDOWS;
    assert!(config.can_retrieve_window_content());
    assert!(!config.can_retrieve_windows());
    let mut host = host_with_bound(config);

    let mut reply = transact(&mut host, GET_WINDOWS, |_| {});
    assert_eq!(reply.read_int32().expect("exception"), 0);
    assert_eq!(reply.read_int32().expect("typed object present"), 0, "null");

    // A node query is still allowed past the capability gate
    // (`isRetrievalAllowingWindowLocked`: the window is in the stack). This
    // fixture registers no `IAccessibilityInteractionConnection`, so the relay
    // still answers "nothing will call you back" (-1) — but it is *not* the
    // capability refusal, which is what this asserts.
    let mut reply = transact(&mut host, FIND_BY_ACCESSIBILITY_ID, |data| {
        data.write_int32(0);
        data.write_int64(0);
        data.write_int32(1);
        data.write_strong_binder_opt(Some(BinderId(java(0x53))));
        data.write_int32(0);
        data.write_int64(7);
        data.write_int32(0);
    });
    assert_eq!(reply.read_int32().expect("exception"), 0);
    let state = a11y_state(&mut host);
    assert!(
        !state
            .lock()
            .expect("a11y state")
            .unsupported
            .iter()
            .any(|u| u.starts_with("findAccessibilityNodeInfoByAccessibilityId denied")),
        "the content capability alone passes the capability gate"
    );
}
