//! W5 acceptance: `IWindowSession.relayout`'s `SurfaceControl` is bound to the
//! compositor-client registry, so stock `BLASTBufferQueue` can create its child
//! layer instead of asking W3's unregistered placeholder binder.
//!
//! The requests are built in the image's order (the same helpers as
//! `window_session_api34.rs`); the reply is decoded with the image's readers and
//! must end exactly. Then `ISurfaceComposerClient.createSurface` is driven with
//! the binder the reply carried — the exact transaction `BLASTBufferQueue`
//! makes in `SurfaceControl::generateSurfaceLocked`.

use acos_parcel::{BinderId, Parcel};
use acos_services::exception::NO_EXCEPTION;
use acos_services::surface_client;
use acos_services::ServiceHost;
use acos_wm::codec::{
    ClientWindowFrames, InsetsSourceControlArray, InsetsState, LayoutParams, MergedConfiguration,
    ParcelCodec, SurfaceControlInfo,
};

const IWINDOW_MANAGER: &str = "android.view.IWindowManager";
const IWINDOW_SESSION: &str = "android.view.IWindowSession";
const ICOMPOSER_CLIENT: &str = "android.gui.ISurfaceComposerClient";

const OPEN_SESSION: u32 = 4;
const ADD_TO_DISPLAY_AS_USER: u32 = 2;
const RELAYOUT: u32 = 5;
const CREATE_SURFACE: u32 = 1;

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

fn add_request(window: BinderId) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
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

fn read_relayout_surface(reply: &mut Parcel) -> SurfaceControlInfo {
    let _result = reply.read_int32().expect("result");
    assert_eq!(reply.read_int32().expect("frames present"), 1);
    let _ = ClientWindowFrames::read_from_parcel(reply).expect("frames");
    assert_eq!(reply.read_int32().expect("configuration present"), 1);
    let _ = MergedConfiguration::read_from_parcel(reply).expect("configuration");
    assert_eq!(reply.read_int32().expect("surface present"), 1);
    let surface = SurfaceControlInfo::read_from_parcel(reply).expect("SurfaceControl");
    assert_eq!(reply.read_int32().expect("insets present"), 1);
    let _ = InsetsState::read_from_parcel(reply).expect("insets");
    assert_eq!(reply.read_int32().expect("controls present"), 1);
    let _ = InsetsSourceControlArray::read_from_parcel(reply).expect("controls");
    assert_eq!(reply.read_int32().expect("out Bundle"), 0);
    assert_eq!(reply.data_avail(), 0, "the reply must end exactly");
    surface
}

/// `ISurfaceComposerClient.createSurface(name, flags, parent, metadata)`
/// exactly as `SurfaceComposerClient::createSurfaceChecked` writes it.
fn create_surface(
    host: &mut ServiceHost,
    client: BinderId,
    name: &str,
    parent: BinderId,
) -> (BinderId, i32) {
    let mut data = Parcel::new();
    data.write_interface_token(ICOMPOSER_CLIENT).expect("token");
    data.write_string16(name).expect("name");
    data.write_int32(0); // flags
    data.write_strong_binder_opt(Some(parent));
    data.write_int32(0); // LayerMetadata: null
    data.set_data_position(0);
    let mut reply = transact(host, client, CREATE_SURFACE, &mut data);
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    assert_eq!(reply.read_int32().expect("parcelable marker"), 1);
    let size = reply.read_int32().expect("parcelable size");
    assert!(size > 0, "CreateSurfaceResult carries an inline size word");
    let handle = reply
        .read_strong_binder()
        .expect("layer handle")
        .expect("non-null layer handle");
    let layer_id = reply.read_int32().expect("layer id");
    let echoed = reply.read_string16().expect("layer name");
    let _hint = reply.read_int32().expect("transform hint");
    assert_eq!(echoed.as_deref(), Some(name));
    assert_eq!(reply.data_avail(), 0, "CreateSurfaceResult ends exactly");
    (handle, layer_id)
}

#[test]
fn relayout_binds_the_window_surface_to_the_compositor_client() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let window = BinderId(0x5701);

    let mut add = transact(
        &mut host,
        session,
        ADD_TO_DISPLAY_AS_USER,
        &mut add_request(window),
    );
    assert_eq!(add.read_int32().expect("exception"), NO_EXCEPTION);

    let mut reply = transact(&mut host, session, RELAYOUT, &mut relayout_request(window));
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    let surface = read_relayout_surface(&mut reply);

    let client = host
        .binder_for_descriptor(ICOMPOSER_CLIENT)
        .expect("the compositor-client node is installed by registry::install");
    assert_eq!(
        surface.client,
        Some(client),
        "relayout's SurfaceControl must carry the real ISurfaceComposerClient binder, not W3's \
         placeholder (which made BLASTBufferQueue::createSurface answer UNKNOWN_BINDER)"
    );
    let layer = surface
        .layer_binder
        .expect("relayout must carry a compositor layer handle");
    assert_eq!(
        host.descriptor_of(layer),
        Some(surface_client::LAYER_HANDLE_DESCRIPTOR),
        "the layer handle must be a registered layer binder, not an id the peer cannot route"
    );
    assert_ne!(
        layer.0, client.0,
        "the layer handle and the client are different nodes"
    );
    assert!(surface.handle.is_some(), "the layer id is on the wire");

    // Now the exact BLAST step: a child surface whose parent is the window
    // layer. This is `SurfaceControl::generateSurfaceLocked` ->
    // `mClient->createSurface("bbq-wrapper", …, mHandle, …)`.
    let (child, child_id) = create_surface(&mut host, client, "bbq-wrapper", layer);
    assert_ne!(child.0, 0);
    assert_eq!(
        child_id,
        surface.handle.unwrap() as i32 + 1,
        "the child layer gets the next compositor layer id"
    );
    assert_eq!(
        host.descriptor_of(child),
        Some(surface_client::LAYER_HANDLE_DESCRIPTOR)
    );

    // A second relayout of the same window must reuse the same layer (the
    // handle pool is bounded) and still answer with the real client.
    let mut reply = transact(&mut host, session, RELAYOUT, &mut relayout_request(window));
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    let again = read_relayout_surface(&mut reply);
    assert_eq!(again.client, Some(client));
    assert_eq!(again.layer_binder, Some(layer));
}
