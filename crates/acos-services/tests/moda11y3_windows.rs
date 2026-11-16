//! MODA11Y3 step 1: `getWindows` / `getWindow` and the relayed
//! `findAccessibilityNodeInfo*` requests.
//!
//! Every assertion runs against the real `ServiceHost` and the real LAYER1
//! window stack: a window is added through `WmsState`, the app registers its
//! `IAccessibilityInteractionConnection` through
//! `IAccessibilityManager.addAccessibilityInteractionConnection`, and the
//! service's `IAccessibilityServiceConnection` transactions are dispatched
//! through `ServiceHost::transact` exactly like the guest's binder does.

use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};
use acos_services::host::ServiceHost;
use acos_services::misc::{A11yService, A11yState};
use acos_services::wms::WmsState;

const MANAGER: &str = "android.view.accessibility.IAccessibilityManager";
const CONNECTION: &str = "android.accessibilityservice.IAccessibilityServiceConnection";
const INTERACTION: &str = "android.view.accessibility.IAccessibilityInteractionConnection";
const PACKAGE: &str = "dev.acos.fixtures.a11yprobe";

/// A wire handle for an app-side (Java) binder, the way the peer encodes one.
fn java(wire: u32) -> u64 {
    1 << 32 | u64::from(wire)
}

fn a11y_state(host: &mut ServiceHost) -> Arc<Mutex<A11yState>> {
    host.ensure(MANAGER)
        .expect("A11yService")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<A11yService>())
        .expect("A11yService")
        .state()
}

/// Add a window to the session's LAYER1 stack.
fn add_window(state: &Arc<Mutex<A11yState>>, window: u64, type_: i32) {
    let wms = state
        .lock()
        .unwrap()
        .windows
        .clone()
        .expect("the host wires the LAYER1 window stack");
    let mut wms = wms.lock().unwrap();
    let session = wms.session;
    let params = acos_wm::codec::LayoutParams {
        type_,
        width: 400,
        height: 400,
        ..acos_wm::codec::LayoutParams::application(PACKAGE, "Probe")
    };
    wms.wm
        .add_to_display_as_user(session, BinderId(window), params, 0)
        .expect("window add");
}

/// Register the app's interaction connection through the real transaction.
fn add_connection(host: &mut ServiceHost, window: u64, connection: u64) -> i32 {
    use acos_services::value::Value;
    let method = acos_aidl::lookup(MANAGER, 7).expect("addAccessibilityInteractionConnection row");
    let args = vec![
        Value::Binder(Some(BinderId(window))),
        Value::Binder(Some(BinderId(java(0x99)))),
        Value::Binder(Some(BinderId(connection))),
        Value::Str(Some(PACKAGE.to_string())),
        Value::Int(0),
    ];
    let mut data = acos_services::codec::build_request(MANAGER, method, &args).expect("request");
    data.set_data_position(0);
    let binder = host
        .check_service("accessibility")
        .unwrap()
        .expect("accessibility binder");
    let mut reply = Parcel::new();
    host.transact(binder, method.code, &mut data, &mut reply, 0)
        .expect("addAccessibilityInteractionConnection");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    reply.read_int32().expect("window id")
}

fn service_transact(host: &mut ServiceHost, code: u32, args: impl FnOnce(&mut Parcel)) -> Parcel {
    let connection = {
        let state = a11y_state(host);
        let mut state = state.lock().unwrap();
        state.bind_service(
            &acos_services::misc::A11yPendingBind {
                component: format!("{PACKAGE}/.ProbeService"),
                package: PACKAGE.to_string(),
                class: format!("{PACKAGE}.ProbeService"),
                token: 0x1A11_0000_0000_0001,
                seq: 0,
                info: acos_services::value::Parcelable::Null,
            },
            java(0x41),
            1,
            0,
            0,
        );
        state.bound_service.as_ref().unwrap().connection
    };
    let connection = if connection == 0 {
        host.accessibility_connection_node().0
    } else {
        connection
    };
    let mut data = Parcel::new();
    data.write_interface_token(CONNECTION).unwrap();
    args(&mut data);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(BinderId(connection), code, &mut data, &mut reply, 0)
        .expect("service transaction");
    reply
}

#[test]
fn get_windows_reports_the_layer1_stack_topmost_first() {
    let mut host = ServiceHost::standard();
    let state = a11y_state(&mut host);
    add_window(&state, 0x1000, acos_wm::layers::TYPE_APPLICATION);
    add_window(&state, 0x2000, acos_wm::layers::TYPE_ACCESSIBILITY_OVERLAY);
    let activity_id = add_connection(&mut host, 0x1000, java(0x51));
    let overlay_id = add_connection(&mut host, 0x2000, java(0x52));

    let mut reply = service_transact(&mut host, 10, |_| {});
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 1, "one typed object present");

    // `WindowListSparseArray.writeToParcel`: one display, its windows topmost
    // first, then the display id.
    assert_eq!(reply.read_int32().unwrap(), 1, "one display");
    assert_eq!(reply.read_int32().unwrap(), 2, "two windows");
    let first = read_window_info_with_class(&mut reply);
    let second = read_window_info_with_class(&mut reply);
    assert_eq!(reply.read_int32().unwrap(), 0, "display id");

    // The accessibility overlay is above the activity (LAYER1 policy layer 31,
    // layer index from the window count), and `getWindows` is topmost first.
    assert_eq!(first.2, 4, "TYPE_ACCESSIBILITY_OVERLAY first");
    assert_eq!(first.0, overlay_id);
    assert_eq!(first.1, 1, "the topmost window has the larger layer");
    assert_eq!(second.2, 1, "TYPE_APPLICATION below");
    assert_eq!(second.0, activity_id);
    assert_eq!(second.1, 0);
    // Both windows are the same 400x400 frame (the WMS centers it), and the
    // focused flag is on the top one.
    assert_eq!(
        (first.3 .2 - first.3 .0, first.3 .3 - first.3 .1),
        (400, 400),
        "the frame is the window's real 400x400 frame"
    );
    assert_eq!(first.3, second.3, "both windows share the activity frame");
    assert!(first.4 & 0x2 != 0, "the overlay holds focus");
    assert!(second.4 & 0x2 == 0, "the activity below does not");
    assert!(second.4 & 0x1 == 0, "only one window is active");

    // `getWindow(id)` answers the same record.
    let mut reply = service_transact(&mut host, 9, |data| data.write_int32(second.0));
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 1, "present");
    let single = read_window_payload(&mut reply);
    assert_eq!(single.0, second.0);
    assert_eq!(single.2, 1);

    // An unknown id is `null`, never a fabricated window.
    let mut reply = service_transact(&mut host, 9, |data| data.write_int32(4242));
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 0, "null window");
}

/// `(id, layer, type, frame, booleanProperties)` from the payload.
fn read_window_info_with_class(
    parcel: &mut Parcel,
) -> (i32, i32, i32, (i32, i32, i32, i32), i32) {
    // `writeParcelable` writes the item's class name.
    assert_eq!(
        parcel.read_string16().unwrap().as_deref(),
        Some("android.view.accessibility.AccessibilityWindowInfo")
    );
    read_window_payload(parcel)
}

/// The payload of one `AccessibilityWindowInfo` (no class name, no presence
/// marker): what `writeTypedObject` carries for `getWindow`.
fn read_window_payload(
    parcel: &mut Parcel,
) -> (i32, i32, i32, (i32, i32, i32, i32), i32) {
    assert_eq!(parcel.read_int32().unwrap(), 0, "display id");
    let type_ = parcel.read_int32().unwrap();
    let layer = parcel.read_int32().unwrap();
    let properties = parcel.read_int32().unwrap();
    let id = parcel.read_int32().unwrap();
    assert_eq!(parcel.read_int32().unwrap(), -1, "parent id");
    assert_eq!(parcel.read_int32().unwrap(), -1, "task id");
    assert_eq!(parcel.read_int32().unwrap(), 4, "one region rect");
    let frame = (
        parcel.read_int32().unwrap(),
        parcel.read_int32().unwrap(),
        parcel.read_int32().unwrap(),
        parcel.read_int32().unwrap(),
    );
    assert_eq!(parcel.read_int32().unwrap(), 0, "no span marker");
    assert_eq!(parcel.read_string16().unwrap(), None, "no title");
    assert_eq!(parcel.read_int64().unwrap(), 0, "anchor id");
    assert_eq!(parcel.read_int64().unwrap(), 0, "transition time");
    assert_eq!(parcel.read_int32().unwrap(), 0, "no child ids");
    let connection_id = parcel.read_int32().unwrap();
    assert_eq!(connection_id, 1, "the service's connection id");
    assert_eq!(parcel.read_int32().unwrap(), -1, "no LocaleList");
    (id, layer, type_, frame, properties)
}

#[test]
fn a_find_request_is_relayed_to_the_window_connection() {
    let mut host = ServiceHost::standard();
    let state = a11y_state(&mut host);
    add_window(&state, 0x1000, acos_wm::layers::TYPE_APPLICATION);
    let window_id = add_connection(&mut host, 0x1000, java(0x51));
    let _ = host.take_deliveries();

    // `findAccessibilityNodeInfoByAccessibilityId(ACTIVE_WINDOW_ID, ROOT_NODE_ID,
    // interactionId=7, callback, flags=0, threadId=3, arguments=null)`.
    let callback = java(0x61);
    let mut reply = service_transact(&mut host, 3, |data| {
        data.write_int32(0x7fff_ffff); // ACTIVE_WINDOW_ID
        data.write_int64(0); // ROOT_NODE_ID
        data.write_int32(7);
        data.write_strong_binder(BinderId(callback));
        data.write_int32(0);
        data.write_int64(3);
        data.write_int32(0); // null Bundle
    });
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(
        reply.read_string16_array().unwrap(),
        Some(vec![Some(PACKAGE.to_string())]),
        "the window's reported package names"
    );

    // The request went to the app's own connection binder, oneway, with the
    // framework's argument order.
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1, "one relayed interaction");
    let delivery = &deliveries[0];
    assert_eq!(delivery.interface, INTERACTION);
    assert_eq!(delivery.code, 1);
    assert_eq!(delivery.target, BinderId(java(0x51)));
    let mut data = delivery.data.clone();
    data.set_data_position(0);
    data.enforce_interface(INTERACTION).unwrap();
    assert_eq!(data.read_int64().unwrap(), 0, "root node id");
    assert_eq!(data.read_int32().unwrap(), 0, "null Region bounds");
    assert_eq!(data.read_int32().unwrap(), 7, "interactionId");
    assert_eq!(
        data.read_strong_binder().unwrap().map(|b| b.0),
        Some(callback)
    );
    assert_eq!(data.read_int32().unwrap(), 0, "flags");
    assert_eq!(data.read_int32().unwrap(), 0, "interrogating pid");
    assert_eq!(data.read_int64().unwrap(), 3, "interrogating tid");
    assert_eq!(data.read_int32().unwrap(), 0, "null MagnificationSpec");
    assert_eq!(data.read_int32().unwrap(), -1, "null matrixValues");
    assert_eq!(data.read_int32().unwrap(), 0, "null Bundle");

    let recorded = state.lock().unwrap();
    assert_eq!(recorded.interactions.len(), 1);
    assert_eq!(recorded.interactions[0].window, window_id);
    assert_eq!(recorded.interactions[0].interaction_id, 7);
}

#[test]
fn a_find_by_text_request_uses_the_text_connection_code() {
    let mut host = ServiceHost::standard();
    let state = a11y_state(&mut host);
    add_window(&state, 0x1000, acos_wm::layers::TYPE_APPLICATION);
    let window_id = add_connection(&mut host, 0x1000, java(0x51));
    let _ = host.take_deliveries();

    // `findAccessibilityNodeInfosByText(windowId, nodeId, text, interactionId,
    // callback, threadId)`.
    let mut reply = service_transact(&mut host, 4, |data| {
        data.write_int32(window_id);
        data.write_int64(0);
        data.write_string16("Send").unwrap();
        data.write_int32(9);
        data.write_strong_binder(BinderId(java(0x61)));
        data.write_int64(4);
    });
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(
        reply.read_string16_array().unwrap(),
        Some(vec![Some(PACKAGE.to_string())])
    );
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].code, 3, "IAccessibilityInteractionConnection by text");
    let mut data = deliveries[0].data.clone();
    data.set_data_position(0);
    data.enforce_interface(INTERACTION).unwrap();
    assert_eq!(data.read_int64().unwrap(), 0);
    assert_eq!(data.read_string16().unwrap().as_deref(), Some("Send"));
    assert_eq!(data.read_int32().unwrap(), 0, "null Region bounds");
    assert_eq!(data.read_int32().unwrap(), 9, "interactionId");
}

#[test]
fn a_window_without_a_connection_is_not_routed() {
    let mut host = ServiceHost::standard();
    let state = a11y_state(&mut host);
    add_window(&state, 0x1000, acos_wm::layers::TYPE_APPLICATION);
    let _ = host.take_deliveries();

    // AOSP answers `null` when no connection is registered for the window: no
    // request goes out and no result is claimed.
    let mut reply = service_transact(&mut host, 3, |data| {
        data.write_int32(0x7fff_ffff);
        data.write_int64(0);
        data.write_int32(1);
        data.write_strong_binder(BinderId(java(0x61)));
        data.write_int32(0);
        data.write_int64(0);
        data.write_int32(0);
    });
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), -1, "null String[]");
    assert!(host.take_deliveries().is_empty(), "nothing was routed");
}

#[test]
fn the_report_names_the_window_stack() {
    let mut host = ServiceHost::standard();
    let state = a11y_state(&mut host);
    add_window(&state, 0x1000, acos_wm::layers::TYPE_APPLICATION);
    let _connection_id = add_connection(&mut host, 0x1000, java(0x51));
    let json = state.lock().unwrap().to_json();
    let windows = json["windows"].as_array().expect("windows array");
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["type"], 1);
    assert_eq!(windows[0]["layer"], 0);
    assert_eq!(windows[0]["focused"], true);
    assert_eq!(windows[0]["package"], PACKAGE);
}

#[test]
fn a_direct_state_without_a_window_source_reports_no_windows() {
    // A bare host (no LAYER1 state) must answer "no windows" instead of
    // inventing a list; `getWindows` still returns an empty WindowListSparseArray.
    let state = Arc::new(Mutex::new(A11yState::new()));
    assert!(state.lock().unwrap().window_snapshot().is_empty());
    let mut host = ServiceHost::standard();
    host.set_accessibility_state(Arc::clone(&state));
    let mut reply = service_transact(&mut host, 10, |_| {});
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 1, "present");
    assert_eq!(reply.read_int32().unwrap(), 1, "one display");
    assert_eq!(reply.read_int32().unwrap(), 0, "no windows");
    assert_eq!(reply.read_int32().unwrap(), 0, "display id");
}

/// Keep the unused-import warning honest: `WmsState` is the type the module
/// reads and the assertion above relies on the host wiring it.
#[allow(dead_code)]
fn _window_source_is_a_wms_state(_: &Arc<Mutex<WmsState>>) {}

#[test]
fn a_perform_action_request_is_relayed_with_the_action_payload() {
    let mut host = ServiceHost::standard();
    let state = a11y_state(&mut host);
    add_window(&state, 0x1000, acos_wm::layers::TYPE_APPLICATION);
    let window_id = add_connection(&mut host, 0x1000, java(0x51));
    let _ = host.take_deliveries();

    // `performAccessibilityAction(windowId, nodeId, ACTION_CLICK=16,
    // arguments=null, interactionId=11, callback, threadId=5)`.
    let callback = java(0x61);
    let mut reply = service_transact(&mut host, 8, |data| {
        data.write_int32(window_id);
        data.write_int64(0x42);
        data.write_int32(16);
        data.write_int32(0); // null Bundle
        data.write_int32(11);
        data.write_strong_binder(BinderId(callback));
        data.write_int64(5);
    });
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 1, "the request went out");

    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].code, 6);
    assert_eq!(deliveries[0].target, BinderId(java(0x51)));
    let mut data = deliveries[0].data.clone();
    data.set_data_position(0);
    data.enforce_interface(INTERACTION).unwrap();
    assert_eq!(data.read_int64().unwrap(), 0x42, "node id");
    assert_eq!(data.read_int32().unwrap(), 16, "action id");
    assert_eq!(data.read_int32().unwrap(), 0, "null Bundle");
    assert_eq!(data.read_int32().unwrap(), 11, "interactionId");
    assert_eq!(
        data.read_strong_binder().unwrap().map(|b| b.0),
        Some(callback)
    );
    assert_eq!(data.read_int32().unwrap(), 0, "flags");
    assert_eq!(data.read_int32().unwrap(), 0, "interrogating pid");
    assert_eq!(data.read_int64().unwrap(), 5, "interrogating tid");
    assert_eq!(state.lock().unwrap().interactions[0].code, 8);
}
