//! MODA11Y4 step 3: `performGlobalAction(GLOBAL_ACTION_BACK)`.
//!
//! The module does not call the activity manager behind the app's back: it
//! publishes a real `KEYCODE_BACK` down/up pair on the focused window's
//! `InputChannel` — the same publish the control plane's `back` uses — and the
//! app's own `Window` decides. On the wire that means two things this test
//! checks directly:
//!
//! * the caller gets AOSP's boolean (`true` = the action was dispatched) and
//!   the key really is on the channel (a `false`/refusal is never turned into a
//!   `true`);
//! * the pair is a plain, untracked press on the wire: `KeyEvent.FLAG_TRACKING`
//!   is *not* set by the publisher. The app process sets it itself —
//!   `Activity.onKeyDown(BACK)` calls `event.startTracking()`, and
//!   `KeyEvent.dispatch`'s up branch runs `DispatcherState.handleUpEvent`
//!   before `onKeyUp`, which is what `Activity.onKeyUp`'s `isTracking()` test
//!   reads (`.cache/aosp/base/core/java/android/view/KeyEvent.java:2960-3100`,
//!   `.../android/app/Activity.java:4018-4136`). Sending the flag would hide a
//!   down the app never tracked; the down must be a first press
//!   (`repeat_count == 0`, no flags) so tracking can start.
//!
//! Every call goes through `ServiceHost::transact`, like the guest's binder.

use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};
use acos_services::host::ServiceHost;
use acos_services::input_channel::{
    InputMessage, LoopbackConsumer, KEYCODE_BACK, KEY_ACTION_DOWN, KEY_ACTION_UP, KEY_FLAG_TRACKING,
};
use acos_services::misc::{A11yService, A11yState};

const MANAGER: &str = "android.view.accessibility.IAccessibilityManager";
const CONNECTION: &str = "android.accessibilityservice.IAccessibilityServiceConnection";

fn a11y_state(host: &mut ServiceHost) -> Arc<Mutex<A11yState>> {
    host.ensure(MANAGER)
        .expect("A11yService")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<A11yService>())
        .expect("A11yService")
        .state()
}

/// Bind a service the way `publishService` + `init` do, so the connection node
/// exists, then dispatch one `IAccessibilityServiceConnection` transaction.
fn service_transact(host: &mut ServiceHost, code: u32, args: impl FnOnce(&mut Parcel)) -> Parcel {
    service_transact_result(host, code, args).expect("service transaction")
}

/// The same call, keeping the host's structured error so a refusal can be
/// asserted as a refusal (never as a fake success).
fn service_transact_result(
    host: &mut ServiceHost,
    code: u32,
    args: impl FnOnce(&mut Parcel),
) -> Result<Parcel, acos_services::ServiceError> {
    let connection = {
        let state = a11y_state(host);
        let mut state = state.lock().unwrap();
        state.bind_service(
            &acos_services::misc::A11yPendingBind {
                component: "dev.acos.fixtures.a11yprobe/.A11yProbeService".to_string(),
                package: "dev.acos.fixtures.a11yprobe".to_string(),
                class: "dev.acos.fixtures.a11yprobe.A11yProbeService".to_string(),
                token: 0x1A11_0000_0000_0001,
                seq: 0,
                info: acos_services::value::Parcelable::Null,
            },
            0x41 << 32 | 0x41,
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
    host.transact(BinderId(connection), code, &mut data, &mut reply, 0)?;
    Ok(reply)
}

#[test]
fn back_is_a_real_key_on_the_focused_window_without_a_tracking_flag() {
    const WINDOW: BinderId = BinderId(0x9001);
    let mut host = ServiceHost::standard();
    let input = a11y_state(&mut host)
        .lock()
        .unwrap()
        .input
        .clone()
        .expect("the host wires the session's input channels");
    let fd = {
        let mut dispatch = input.state().lock().unwrap();
        dispatch.register_window(WINDOW, "activity").expect("register");
        dispatch.set_focus(Some(WINDOW)).expect("focus");
        dispatch.duplicate_client_fd(WINDOW).expect("client fd")
    };
    let mut guest = LoopbackConsumer::new(fd);

    // `IAccessibilityServiceConnection.performGlobalAction(int)`: AOSP's
    // `GLOBAL_ACTION_BACK` is 1.
    let mut reply = service_transact(&mut host, 12, |data| data.write_int32(1));
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 1, "the action was dispatched");

    // The channel carries the press: DOWN then UP, both `KEYCODE_BACK`, and the
    // up is the tracked one. (Registration already published TOUCH_MODE and
    // FOCUS, so skip everything that is not a key.)
    let mut keys = Vec::new();
    while let Some(message) = guest.recv().expect("recv") {
        if let InputMessage::Key { event, .. } = message {
            keys.push(event);
            if keys.len() == 2 {
                break;
            }
        }
    }
    assert_eq!(keys.len(), 2, "a down/up pair was published");
    assert_eq!(keys[0].action, KEY_ACTION_DOWN);
    assert_eq!(keys[0].key_code, KEYCODE_BACK);
    assert_eq!(
        keys[0].flags, 0,
        "the down is a first press with no flags: FLAG_START_TRACKING is set by \
         Activity.onKeyDown in the app, not by the dispatcher"
    );
    assert_eq!(
        keys[0].repeat_count, 0,
        "KeyEvent.dispatch only starts tracking when repeatCount == 0"
    );
    assert_eq!(keys[1].action, KEY_ACTION_UP);
    assert_eq!(keys[1].key_code, KEYCODE_BACK);
    assert_eq!(
        keys[1].flags & KEY_FLAG_TRACKING,
        0,
        "the publisher must not pre-set FLAG_TRACKING: the app's own \
         KeyEvent.DispatcherState sets it on the up of a tracked down"
    );
    assert_eq!(
        keys[0].down_time, keys[1].down_time,
        "down and up of one press share downTime (KeyEvent.getDownTime())"
    );
}

/// MODA11Y5: the key goes to the window with input **focus**, not to the most
/// recently added window. A `FLAG_NOT_FOCUSABLE` `TYPE_ACCESSIBILITY_OVERLAY`
/// window added after the activity must not swallow Back — AOSP's
/// `InputDispatcher::dispatchKeyLocked` only ever dispatches a key to
/// `getFocusedWindowHandle`. Before the fix the module used the newest token,
/// so an overlay added by the service took every Back silently.
#[test]
fn back_goes_to_the_focused_window_not_the_newest_one() {
    const ACTIVITY: BinderId = BinderId(0x9101);
    const OVERLAY: BinderId = BinderId(0x9102);
    let mut host = ServiceHost::standard();
    let input = a11y_state(&mut host)
        .lock()
        .unwrap()
        .input
        .clone()
        .expect("the host wires the session's input channels");
    let (activity_fd, overlay_fd) = {
        let mut dispatch = input.state().lock().unwrap();
        dispatch.register_window(ACTIVITY, "activity").expect("register");
        dispatch.register_window(OVERLAY, "a11y overlay").expect("register");
        // The WMS layer stack: the activity keeps focus, the overlay is
        // FLAG_NOT_FOCUSABLE (`layer_stack().focus_target()`).
        dispatch.set_focus(Some(ACTIVITY)).expect("focus");
        (
            dispatch.duplicate_client_fd(ACTIVITY).expect("activity fd"),
            dispatch.duplicate_client_fd(OVERLAY).expect("overlay fd"),
        )
    };
    let mut activity = LoopbackConsumer::new(activity_fd);
    let mut overlay = LoopbackConsumer::new(overlay_fd);

    let mut reply = service_transact(&mut host, 12, |data| data.write_int32(1));
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 1, "the action was dispatched");

    let mut keys = Vec::new();
    while let Some(message) = activity.recv().expect("recv") {
        if let InputMessage::Key { event, .. } = message {
            keys.push(event);
            if keys.len() == 2 {
                break;
            }
        }
    }
    assert_eq!(keys.len(), 2, "the focused window receives the pair");
    assert_eq!(keys[0].key_code, KEYCODE_BACK);
    assert_eq!(keys[1].key_code, KEYCODE_BACK);

    // The overlay's channel carries no key at all: a non-focusable window never
    // sees Back.
    let mut overlay_keys = 0;
    while let Some(message) = overlay.recv().expect("recv") {
        if let InputMessage::Key { .. } = message {
            overlay_keys += 1;
        }
    }
    assert_eq!(overlay_keys, 0, "the overlay must not receive the key");
}

#[test]
fn back_without_a_live_window_is_a_structured_refusal() {
    let mut host = ServiceHost::standard();
    // No window has registered an input channel yet: the honest answer is an
    // error, never `true` over a key that was never published.
    let error = service_transact_result(&mut host, 12, |data| data.write_int32(1))
        .expect_err("BACK without a live window must be refused");
    assert!(
        format!("{error}").contains("no focused window input channel"),
        "the refusal names the cause: {error}"
    );
}

#[test]
fn other_global_actions_stay_structured_refusals() {
    let mut host = ServiceHost::standard();
    // `GLOBAL_ACTION_HOME` is 2: it needs system UI, which the session does not
    // have. A `true` here would be a lie.
    let error = service_transact_result(&mut host, 12, |data| data.write_int32(2))
        .expect_err("HOME must stay a structured refusal");
    assert!(
        format!("{error}").contains("system-UI actions"),
        "the refusal says why: {error}"
    );
    assert!(
        a11y_state(&mut host)
            .lock()
            .unwrap()
            .unsupported
            .iter()
            .any(|entry| entry.contains("performGlobalAction(2)")),
        "the refusal is recorded for the module report"
    );
}
