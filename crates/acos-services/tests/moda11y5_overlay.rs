//! MODA11Y5 step 4: `TYPE_ACCESSIBILITY_OVERLAY` windows.
//!
//! AOSP lets a window into the accessibility overlay layer only when it is
//! added on the accessibility manager's own overlay window token: `addWindow`
//! answers `ADD_BAD_APP_TOKEN` for a `TYPE_ACCESSIBILITY_OVERLAY` window whose
//! token is not one (`WindowManagerService.java:1630-1640`), and
//! `sanitizeWindowType` only assigns that type to such a token
//! (`WindowManagerService.java:9914-9928`). The token reaches the service in
//! `IAccessibilityServiceClient.init(connectionId, windowToken)`
//! (`AccessibilityService.java:2803-2812`), which is the token the framework
//! then fills into the service's `WindowManager.LayoutParams`
//! (`AccessibilityService.java:2660-2670`).
//!
//! This file proves the host half: the bind registers that token with the
//! window manager, the add is accepted on it and refused on anything else, and
//! the window lands in the LAYER1 accessibility-overlay policy layer (31).

use std::sync::{Arc, Mutex};

use acos_parcel::binder::BinderId;
use acos_services::host::ServiceHost;
use acos_services::misc::{A11yPendingBind, A11yService, A11yState};
use acos_services::value::Parcelable;
use acos_wm::layers::{TYPE_ACCESSIBILITY_OVERLAY, TYPE_APPLICATION};
use acos_wm::wms::{ADD_BAD_APP_TOKEN, ADD_OKAY};

const OVERLAY_TOKEN: u64 = 0x0A11_0000_0000_7001;

fn a11y_state(host: &mut ServiceHost) -> Arc<Mutex<A11yState>> {
    host.ensure("android.view.accessibility.IAccessibilityManager")
        .expect("A11yService")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<A11yService>())
        .expect("A11yService")
        .state()
}

fn pending_bind() -> A11yPendingBind {
    A11yPendingBind {
        component: "dev.acos.fixtures.a11yprobe/.A11yProbeService".to_string(),
        package: "dev.acos.fixtures.a11yprobe".to_string(),
        class: "dev.acos.fixtures.a11yprobe.A11yProbeService".to_string(),
        token: 0x1A11_0000_0000_0001,
        seq: 0,
        info: Parcelable::Null,
    }
}

/// The same add the app's `WindowManagerImpl.addView` makes, through the real
/// `IWindowSession` glue.
fn add_window(state: &Arc<Mutex<A11yState>>, window: u64, type_: i32, token: Option<u64>) -> i32 {
    let wms = state
        .lock()
        .unwrap()
        .windows
        .clone()
        .expect("the host wires the LAYER1 window stack");
    let mut wms = wms.lock().unwrap();
    let session = wms.session;
    let mut params = acos_wm::codec::LayoutParams {
        type_,
        width: 240,
        height: 120,
        ..acos_wm::codec::LayoutParams::application(PACKAGE, "overlay")
    };
    params.token = token.map(BinderId);
    wms.wm
        .add_to_display_as_user(session, BinderId(window), params, 0)
        .expect("the add path answers")
        .result
}

const PACKAGE: &str = "dev.acos.fixtures.a11yprobe";

#[test]
fn the_bind_registers_the_overlay_window_token_and_wms_honours_it() {
    let mut host = ServiceHost::standard();
    let state = a11y_state(&mut host);

    // Before the bind there is no overlay authority at all: the add is refused
    // with AOSP's `ADD_BAD_APP_TOKEN`, not silently accepted.
    assert_eq!(
        add_window(&state, 0x5001, TYPE_ACCESSIBILITY_OVERLAY, Some(OVERLAY_TOKEN)),
        ADD_BAD_APP_TOKEN,
        "no bound service, no overlay token"
    );

    // The bind the module makes: `publishService` hands the app's
    // `IAccessibilityServiceClient` its `init(connectionId, windowToken)`.
    {
        let shared = a11y_state(&mut host);
        let mut state = shared.lock().unwrap();
        state.bind_service(&pending_bind(), 0x41 << 32 | 0x41, 1, 0, OVERLAY_TOKEN);
        state.register_overlay_token(OVERLAY_TOKEN);
        assert!(state.bound_service.is_some(), "the service is bound");
    }

    // Now the service's own overlay add is accepted, and it lands in policy
    // layer 31 (LAYER1), above the activity.
    assert_eq!(
        add_window(&state, 0x5002, TYPE_ACCESSIBILITY_OVERLAY, Some(OVERLAY_TOKEN)),
        ADD_OKAY,
        "the bound service's token authorises the overlay"
    );
    assert_eq!(add_window(&state, 0x5003, TYPE_APPLICATION, None), ADD_OKAY);
    {
        let wms = state.lock().unwrap().windows.clone().unwrap();
        let wms = wms.lock().unwrap();
        let order: Vec<(u64, i32)> = wms
            .wm
            .layer_stack()
            .ordered()
            .iter()
            .map(|entry| (entry.window, entry.type_))
            .collect();
        assert_eq!(
            order,
            vec![
                (0x5003, TYPE_APPLICATION),
                (0x5002, TYPE_ACCESSIBILITY_OVERLAY)
            ],
            "the overlay is above the application window in the layer stack"
        );
    }

    // A different token is refused even while a service is bound: the authority
    // is the token, not "some accessibility service exists".
    assert_eq!(
        add_window(&state, 0x5004, TYPE_ACCESSIBILITY_OVERLAY, Some(0xBAD0_0000)),
        ADD_BAD_APP_TOKEN,
        "a foreign token cannot claim the accessibility overlay layer"
    );
    // A window type that is not `TYPE_ACCESSIBILITY_OVERLAY` still keeps the
    // pre-existing behaviour (the add path has no token policy for it yet), so
    // this test's claim stays exactly the accessibility one.
    assert_eq!(
        add_window(&state, 0x5005, TYPE_APPLICATION, Some(0xBAD0_0000)),
        ADD_OKAY
    );

    // Turning accessibility off removes the token authority with the service.
    a11y_state(&mut host).lock().unwrap().disable();
    assert_eq!(
        add_window(&state, 0x5006, TYPE_ACCESSIBILITY_OVERLAY, Some(OVERLAY_TOKEN)),
        ADD_BAD_APP_TOKEN,
        "a disabled module has no overlay token"
    );
}
