//! MODA11Y6 step 1: a bound accessibility service must hear about the window
//! stack, the way AOSP's accessibility window manager reports it.
//!
//! The failure this pins: `AccessibilityService.getWindows()` answers from the
//! guest-side `AccessibilityCache` on a hit
//! (`AccessibilityInteractionClient.getWindowsOnAllDisplays`,
//! `AccessibilityInteractionClient.java:489-532`), and that cache is only
//! cleared by an accessibility event (`AccessibilityCache.onAccessibilityEvent`
//! clears it for `TYPE_WINDOWS_CHANGED`, `AccessibilityCache.java:276-288`).
//! AOSP emits one `TYPE_WINDOWS_CHANGED` per added/removed/changed window
//! (`AccessibilityWindowManager.java:940-975`,
//! `AccessibilityEvent.obtainWindowsChangedEvent`) and delivers it even to a
//! service whose `eventTypes` do not list it, because it is cache-critical
//! (`AbstractAccessibilityServiceConnection.notifyAccessibilityEvent`:
//! `requiredForCacheConsistency`).
//!
//! Before this change a second `getWindows()` after the probe's overlay add
//! returned the stale cached list (one window, no overlay) with no IPC at all.
//!
//! The second test is the concurrency one the brief asks for: the session must
//! not hold the window-manager state while it calls out to the sink (a call
//! that can wait on another component or the guest), because
//! `A11yState::window_snapshot` takes the window state and would then block
//! behind it.

use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use acos_parcel::{BinderId, Parcel};
use acos_services::host::ServiceHost;
use acos_services::misc::{A11yPendingBind, A11yService, A11yState};
use acos_services::value::Parcelable;
use acos_services::window_session::{WindowSessionService, WindowStackChange, WindowStackSink};
use acos_wm::codec::{LayoutParams, ParcelCodec};

const MANAGER: &str = "android.view.accessibility.IAccessibilityManager";
const IWINDOW_MANAGER: &str = "android.view.IWindowManager";
const IWINDOW_SESSION: &str = "android.view.IWindowSession";
const CLIENT: &str = "android.accessibilityservice.IAccessibilityServiceClient";
const PACKAGE: &str = "dev.acos.fixtures.a11yprobe";

const OPEN_SESSION: u32 = 4;
const ADD_TO_DISPLAY_AS_USER: u32 = 2;
const REMOVE: u32 = 4;

/// `AccessibilityEvent.TYPE_WINDOWS_CHANGED` (`1 << 22`).
const TYPE_WINDOWS_CHANGED: i32 = 1 << 22;
/// `AccessibilityEvent.WINDOWS_CHANGE_ADDED` / `WINDOWS_CHANGE_REMOVED`.
const WINDOWS_CHANGE_ADDED: i32 = 1;
const WINDOWS_CHANGE_REMOVED: i32 = 1 << 1;

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

/// Bind `client` as the enabled service (the state `publishService` +
/// `IAccessibilityServiceClient.init` leave behind).
///
/// A bound service is an **enabled** service, so the two `Settings.Secure` keys
/// the Settings UI writes are written here too: since GATENBOOT2 the enable set
/// in the settings table is the authority the system reconciles the bind
/// against (`a11y_bind::sync_enabled_services`), and a state that claims a bound
/// service while the table says "off" is the inconsistent one.
fn bind_service(state: &Arc<Mutex<A11yState>>, client: u64) {
    if let Some(settings) = state.lock().expect("a11y state").settings.clone() {
        let mut settings = settings.lock().expect("settings");
        settings.put(
            acos_services::provider::Namespace::Secure,
            "enabled_accessibility_services",
            &format!("{PACKAGE}/.A11yProbeService"),
        );
        settings.put(
            acos_services::provider::Namespace::Secure,
            "accessibility_enabled",
            "1",
        );
    }
    state.lock().expect("a11y state").bind_service(
        &A11yPendingBind {
            component: format!("{PACKAGE}/.A11yProbeService"),
            package: PACKAGE.to_string(),
            class: format!("{PACKAGE}.A11yProbeService"),
            token: 0x1A11_0000_0000_0001,
            seq: 0,
            info: Parcelable::Null,
        },
        client,
        1,
        0,
        0,
    );
}

fn transact(host: &mut ServiceHost, binder: BinderId, code: u32, data: &mut Parcel) -> Parcel {
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, 0)
        .expect("transaction");
    reply.set_data_position(0);
    reply
}

fn open_session(host: &mut ServiceHost) -> BinderId {
    let window = host
        .binder_for_descriptor(IWINDOW_MANAGER)
        .expect("window service");
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_MANAGER).expect("token");
    data.write_strong_binder_opt(None);
    data.set_data_position(0);
    let mut reply = transact(host, window, OPEN_SESSION, &mut data);
    assert_eq!(reply.read_int32().expect("exception"), 0);
    reply
        .read_strong_binder()
        .expect("session binder")
        .expect("non-null IWindowSession")
}

fn add_request(window: BinderId) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
    data.write_int32(1); // typed LayoutParams present
    LayoutParams::application(PACKAGE, "MainActivity")
        .write_to_parcel(&mut data)
        .expect("LayoutParams");
    data.write_int32(0); // viewVisibility
    data.write_int32(-1); // layerStackId
    data.write_int32(0); // userId
    data.write_int32(0); // requestedVisibleTypes
    data.write_int32(0); // out float[] length
    data.set_data_position(0);
    data
}

fn remove_request(window: BinderId) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
    data.set_data_position(0);
    data
}

/// The `IAccessibilityServiceClient.onAccessibilityEvent` deliveries of a
/// batch, with the event fields this test reads.
struct Event {
    target: BinderId,
    code: u32,
    method: String,
    sealed: i32,
    event_type: i32,
    window_changes: i32,
    connection_id: i32,
    source_window_id: i32,
    source_display_id: i32,
    record_sealed: i32,
    record_count: i32,
    service_wants_event: i32,
}

fn accessibility_events(deliveries: &[acos_services::host::PendingDelivery]) -> Vec<Event> {
    let mut out = Vec::new();
    for delivery in deliveries {
        if delivery.interface != CLIENT {
            continue;
        }
        let mut data = delivery.data.clone();
        data.set_data_position(0);
        // The interface token is `policy, work source uid, header, String16`.
        for _ in 0..3 {
            data.read_int32().expect("token header");
        }
        assert_eq!(
            data.read_string16().expect("token").as_deref(),
            Some(CLIENT)
        );
        assert_eq!(
            data.read_int32().expect("typed object present"),
            1,
            "writeTypedObject presence marker"
        );
        let sealed = data.read_int32().expect("sealed");
        let event_type = data.read_int32().expect("eventType");
        let _movement = data.read_int32().expect("movementGranularity");
        let _action = data.read_int32().expect("action");
        let _content = data.read_int32().expect("contentChangeTypes");
        let window_changes = data.read_int32().expect("windowChangeTypes");
        let _speech = data.read_int32().expect("speechStateChangeTypes");
        // `TextUtils.writeToParcel`: `int 1` (not spanned) then a null String8.
        assert_eq!(
            data.read_int32().expect("packageName kind"),
            1,
            "a non-Spanned CharSequence writes kind 1"
        );
        assert_eq!(data.read_string8().expect("packageName"), None);
        let _event_time = data.read_int64().expect("eventTime");
        let connection_id = data.read_int32().expect("record connectionId");
        let _boolean_properties = data.read_int32().expect("booleanProperties");
        for field in 0..12 {
            data.read_int32()
                .unwrap_or_else(|_| panic!("record int {field}"));
        }
        // className / contentDescription / beforeText: three null
        // CharSequences, then `writeParcelable(null)` (a null String16).
        for field in 0..3 {
            assert_eq!(
                data.read_int32().unwrap_or_else(|_| panic!("char {field}")),
                1,
                "a non-Spanned CharSequence writes kind 1"
            );
            assert_eq!(data.read_string8().expect("null CharSequence"), None);
        }
        assert_eq!(data.read_string16().expect("parcelableData"), None);
        assert_eq!(data.read_int32().expect("text list size"), 0);
        let source_window_id = data.read_int32().expect("sourceWindowId");
        let _source_node_id = data.read_int64().expect("sourceNodeId");
        let source_display_id = data.read_int32().expect("sourceDisplayId");
        let record_sealed = data.read_int32().expect("record sealed");
        let record_count = data.read_int32().expect("record count");
        let service_wants_event = data.read_int32().expect("serviceWantsEvent");
        out.push(Event {
            target: delivery.target,
            code: delivery.code,
            method: delivery.method.clone(),
            sealed,
            event_type,
            window_changes,
            connection_id,
            source_window_id,
            source_display_id,
            record_sealed,
            record_count,
            service_wants_event,
        });
    }
    out
}

#[test]
fn a_window_add_and_remove_deliver_type_windows_changed_to_the_bound_service() {
    let mut host = ServiceHost::standard();
    let state = a11y_state(&mut host);
    let client = java(0x41);
    bind_service(&state, client);
    let session = open_session(&mut host);
    let window = BinderId(0x6001);

    let mut reply = transact(
        &mut host,
        session,
        ADD_TO_DISPLAY_AS_USER,
        &mut add_request(window),
    );
    assert_eq!(reply.read_int32().expect("exception"), 0);
    let events = accessibility_events(&host.take_deliveries());
    assert_eq!(
        events.len(),
        1,
        "one TYPE_WINDOWS_CHANGED per added window; got {:?}",
        events
            .iter()
            .map(|e| (e.event_type, e.window_changes, e.source_window_id))
            .collect::<Vec<_>>()
    );
    let added = &events[0];
    assert_eq!(added.target, BinderId(client), "the bound service's client");
    assert_eq!(
        added.code, 2,
        "onAccessibilityEvent is the second oneway call"
    );
    assert_eq!(added.method, "onAccessibilityEvent");
    assert_eq!(added.sealed, 1, "AMS seals every delivered event");
    assert_eq!(added.event_type, TYPE_WINDOWS_CHANGED);
    assert_eq!(added.window_changes, WINDOWS_CHANGE_ADDED);
    assert_eq!(added.connection_id, 1, "the service's connection id");
    assert_eq!(
        added.source_window_id, 1,
        "the accessibility window id assigned to the added window"
    );
    assert_eq!(added.source_display_id, -1, "Display.INVALID_DISPLAY");
    assert_eq!(added.record_sealed, 0);
    assert_eq!(added.record_count, 0);
    assert_eq!(added.service_wants_event, 1);

    // A removal is the same event with the REMOVED mask.
    let mut reply = transact(&mut host, session, REMOVE, &mut remove_request(window));
    assert_eq!(reply.read_int32().expect("exception"), 0);
    let events = accessibility_events(&host.take_deliveries());
    assert_eq!(events.len(), 1, "one event per removed window");
    assert_eq!(events[0].event_type, TYPE_WINDOWS_CHANGED);
    assert_eq!(events[0].window_changes, WINDOWS_CHANGE_REMOVED);
    assert_eq!(events[0].source_window_id, 1, "the same window id");
}

#[test]
fn no_service_no_window_change_delivery() {
    // An unbound service cannot observe windows; the session must not invent a
    // target for the event.
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let mut reply = transact(
        &mut host,
        session,
        ADD_TO_DISPLAY_AS_USER,
        &mut add_request(BinderId(0x6002)),
    );
    assert_eq!(reply.read_int32().expect("exception"), 0);
    assert!(
        accessibility_events(&host.take_deliveries()).is_empty(),
        "no bound service, no onAccessibilityEvent"
    );
}

/// The brief's step-1 concurrency test: a window transaction that is stalled
/// inside the window-stack sink must not block `getWindows`' window snapshot.
///
/// The sink stands in for a call that waits on another component (in
/// production: the accessibility state, and behind it the guest). Before the
/// fix `raw_transact` held the window-manager state lock for the whole
/// transaction, so a `window_snapshot` — what the `getWindows` handler does —
/// blocked until the stalled call returned.
#[test]
fn a_stalled_window_change_sink_does_not_block_the_window_snapshot() {
    let mut host = ServiceHost::standard();
    let state = a11y_state(&mut host);
    bind_service(&state, java(0x42));
    let session = open_session(&mut host);

    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    // `Receiver` is not `Sync`, so the sink waits on a mutex-guarded copy.
    let release_rx = Mutex::new(release_rx);
    let sink: WindowStackSink = Arc::new(move |_change: WindowStackChange| {
        let _ = entered_tx.send(());
        // Bounded so a failing build reports instead of hanging the suite.
        let receiver = release_rx.lock().expect("release receiver");
        let _ = receiver.recv_timeout(Duration::from_millis(1500));
        Vec::new()
    });
    host.dynamic_object_mut(IWINDOW_SESSION)
        .expect("IWindowSession node")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<WindowSessionService>())
        .expect("WindowSessionService")
        .set_window_changes(sink);

    let snapshot_state = Arc::clone(&state);
    let adder = std::thread::spawn(move || {
        let mut data = add_request(BinderId(0x6003));
        let mut reply = Parcel::new();
        host.transact(session, ADD_TO_DISPLAY_AS_USER, &mut data, &mut reply, 0)
            .expect("add");
        reply.set_data_position(0);
        reply.read_int32().expect("exception")
    });

    // The add is inside the sink: the window state must be free.
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the sink was reached");
    let started = Instant::now();
    let windows = snapshot_state.lock().expect("a11y state").window_snapshot();
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_millis(500),
        "window_snapshot waited {waited:?} behind the stalled sink"
    );
    assert_eq!(windows.len(), 1, "the added window is already in the stack");

    release_tx.send(()).expect("release the sink");
    assert_eq!(adder.join().expect("adder"), 0, "the add succeeded");
}
