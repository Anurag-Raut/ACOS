//! W6 acceptance: the host delivers `IWindow.dispatchAppVisibility(true)` to the
//! app's own `IWindow` binder, and **only** that callback.
//!
//! `ViewRootImpl` derives `mAppVisible` from the `addToDisplayAsUser` result
//! (`ADD_FLAG_APP_VISIBLE`), which ACOS does not set, so `getHostVisibility()`
//! is `GONE` and `performDraw` is skipped until the real WMS's
//! `dispatchAppVisibility(true)` arrives (W5 §5.1). The same queue also holds
//! `windowFocusChanged` (code 17 — not a transaction in the API-34 `IWindow`)
//! and the post-layout `resized`/`insetsControlChanged`; W5's attempt to flush
//! all of them from both add and relayout hung the guest, so these tests pin the
//! narrow shape:
//!
//! 1. one delivery per window, code 7, `android.view.IWindow` token, one `true`;
//! 2. nothing at all from `relayout` (the hang shape);
//! 3. no code-17 delivery ever.

use acos_parcel::{BinderId, Parcel};
use acos_services::exception::NO_EXCEPTION;
use acos_services::ServiceHost;
use acos_wm::codec::{LayoutParams, ParcelCodec};

const IWINDOW_MANAGER: &str = "android.view.IWindowManager";
const IWINDOW_SESSION: &str = "android.view.IWindowSession";
const IWINDOW: &str = "android.view.IWindow";

const OPEN_SESSION: u32 = 4;
const ADD_TO_DISPLAY_AS_USER: u32 = 2;
const RELAYOUT: u32 = 5;
const DISPATCH_APP_VISIBILITY: u32 = 7;

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
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    reply
        .read_strong_binder()
        .expect("session binder")
        .expect("non-null IWindowSession")
}

fn add_request(window: Option<BinderId>) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(window);
    data.write_int32(1);
    LayoutParams::application("dev.acos.fixtures.counter", "MainActivity")
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

fn relayout_request(window: BinderId) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
    data.write_int32(0); // attrs null: keep the stored params
    data.write_int32(-1); // requestedWidth
    data.write_int32(-1); // requestedHeight
    data.write_int32(0); // viewVisibility
    data.write_int32(0); // flags
    data.write_int32(0); // seq
    data.write_int32(-1); // lastSyncSeqId
    data.set_data_position(0);
    data
}

fn add(host: &mut ServiceHost, session: BinderId, window: BinderId) {
    let mut reply = transact(
        host,
        session,
        ADD_TO_DISPLAY_AS_USER,
        &mut add_request(Some(window)),
    );
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
}

#[test]
fn add_to_display_delivers_one_dispatch_app_visibility_to_the_window_binder() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let window = BinderId(0x5701);

    add(&mut host, session, window);

    let deliveries = host.take_deliveries();
    assert_eq!(
        deliveries.len(),
        1,
        "exactly one reverse transaction: dispatchAppVisibility(true); got {:?}",
        deliveries
            .iter()
            .map(|d| (d.interface.clone(), d.code, d.method.clone()))
            .collect::<Vec<_>>()
    );
    let d = &deliveries[0];
    assert_eq!(
        d.target, window,
        "addressed at the app's own IWindow binder"
    );
    assert_eq!(d.interface, IWINDOW);
    assert_eq!(d.code, DISPATCH_APP_VISIBILITY);
    assert_eq!(d.method, "dispatchAppVisibility");

    // The body is the framework's `IWindow$Stub` argument list: interface token
    // (already written) + one bool, and nothing else.
    let mut body = d.data.clone();
    body.set_data_position(0);
    body.enforce_interface(IWINDOW)
        .expect("android.view.IWindow token");
    assert_eq!(body.read_bool().expect("visible"), true);
    body.enforce_no_data_avail().expect("no trailing bytes");

    // Nothing may be addressed at handle 0: a rejected add has no binder.
    assert!(deliveries.iter().all(|d| d.target.0 != 0));
}

#[test]
fn relayout_delivers_nothing_and_a_repeated_add_does_not_re_deliver() {
    // The teeth against W5's hung attempt: that shape flushed the queue from
    // `relayout` too (resized(reportDraw=true) + insetsControlChanged), which
    // produced a traversal storm and `outcome=timeout`.
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let window = BinderId(0x5701);

    add(&mut host, session, window);
    assert_eq!(
        host.take_deliveries().len(),
        1,
        "the first add delivers once"
    );

    let mut reply = transact(&mut host, session, RELAYOUT, &mut relayout_request(window));
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    assert!(
        host.take_deliveries().is_empty(),
        "relayout must not put resized/insetsControlChanged on the wire (W5 §5.1)"
    );

    // A second add re-queues the visibility callback in `acos-wm`; the host must
    // not deliver it twice (the window is already visible).
    add(&mut host, session, window);
    assert!(
        host.take_deliveries().is_empty(),
        "dispatchAppVisibility is delivered once per window"
    );
}

#[test]
fn a_null_window_add_delivers_nothing() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let mut reply = transact(
        &mut host,
        session,
        ADD_TO_DISPLAY_AS_USER,
        &mut add_request(None),
    );
    // The add policy rejects a null window; the reply still starts with the
    // real exception header rule (either a service exception or a result).
    let _ = reply.read_int32().expect("exception-or-result");
    assert!(
        host.take_deliveries().is_empty(),
        "no binder to address means no delivery, never a fabricated handle"
    );
}

#[test]
fn no_delivery_ever_uses_the_replay_only_window_focus_code() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let window = BinderId(0x5702);
    add(&mut host, session, window);
    let mut reply = transact(&mut host, session, RELAYOUT, &mut relayout_request(window));
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    for d in host.take_deliveries() {
        assert_ne!(
            d.code, 17,
            "windowFocusChanged (17) is not a transaction in the API-34 IWindow; the stub would \
             throw (results/art-runtime/W5.md §5.1)"
        );
    }
}
