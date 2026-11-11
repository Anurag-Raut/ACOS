//! W-R T2 acceptance: the frame clock the real `Choreographer` needs.
//!
//! Every request below is built **by hand, in the order the image's generated
//! client writes it**, and every reply is decoded in the image's own read order
//! (`data_avail() == 0` at the end). The evidence for each shape is the API-34
//! image's `libgui.so`
//! (`.cache/art/sysroot/system/lib64/libgui.so`), not the `android-14.0.0_r75`
//! AIDL — the image disagrees with the tag for the tail of `ISurfaceComposer`
//! (`getMaxAcquiredBufferCount` is 57, not 65) and has no
//! `IDisplayEventConnection.getSchedulingPolicy` at all.
//!
//! ```text
//! ISurfaceComposer.createDisplayEventConnection (code 2) request:
//!   writeInterfaceToken("android.gui.ISurfaceComposer")
//!   writeInt(vsyncSource) writeInt(eventRegistration) writeStrongBinder(layer)
//! reply:
//!   readInt()=0 (binder::Status)  readStrongBinder()=IDisplayEventConnection
//!
//! IDisplayEventConnection.stealReceiveChannel (code 1) request:
//!   writeInterfaceToken("android.gui.IDisplayEventConnection")   -- nothing else
//! reply:
//!   readInt()=0  readFileDescriptor()  readFileDescriptor()      -- TWO fds
//!
//! IDisplayEventConnection.requestNextVsync (code 3): oneway, no reply body.
//! IDisplayEventConnection.getLatestVsyncEventData (code 4) reply:
//!   readInt()=0  readInt()=1 (Parcel::writeParcelable's non-null flag)
//!   readInt64(frameInterval) readUint32(preferred) readUint32(length)
//!   then 3 x readInt64 per timeline
//! ```

use std::time::{Duration, Instant};

use acos_parcel::{BinderId, Parcel};
use acos_services::display_events::{
    self, decode_event, BitTube, ConnectionClock, EVENT_SIZE, FRAME_TIMELINES_CAPACITY,
    PERIOD_NS_60HZ,
};
use acos_services::exception::NO_EXCEPTION;
use acos_services::{ServiceError, ServiceHost};

const ISURFACE_COMPOSER: &str = "android.gui.ISurfaceComposer";
const IDISPLAY_EVENT_CONNECTION: &str = "android.gui.IDisplayEventConnection";

// Image (`libgui.so` proxy immediates) transaction codes.
const CREATE_DISPLAY_EVENT_CONNECTION: u32 = 2;
const CREATE_CONNECTION: u32 = 3;
const GET_PHYSICAL_DISPLAY_IDS: u32 = 6;
const GET_PHYSICAL_DISPLAY_TOKEN: u32 = 7;
const GET_SUPPORTED_FRAME_TIMESTAMPS: u32 = 8;
const GET_DISPLAY_STATS: u32 = 10;
const GET_DISPLAY_STATE: u32 = 11;
const GET_MAX_ACQUIRED_BUFFER_COUNT: u32 = 57;

const STEAL_RECEIVE_CHANNEL: u32 = 1;
const SET_VSYNC_RATE: u32 = 2;
const REQUEST_NEXT_VSYNC: u32 = 3;
const GET_LATEST_VSYNC_EVENT_DATA: u32 = 4;

const ONEWAY: u32 = 1;

fn transact(
    host: &mut ServiceHost,
    binder: BinderId,
    code: u32,
    flags: u32,
    data: &mut Parcel,
) -> std::result::Result<Parcel, ServiceError> {
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, flags)?;
    reply.set_data_position(0);
    Ok(reply)
}

fn no_exception(reply: &mut Parcel) {
    assert_eq!(reply.read_int32().expect("exception header"), NO_EXCEPTION);
}

fn composer(host: &mut ServiceHost) -> BinderId {
    host.check_service("SurfaceFlinger")
        .expect("lookup is infallible")
        .expect("SurfaceFlinger resolves to the compositor node")
}

/// `BpSurfaceComposer::createDisplayEventConnection`'s request, exactly.
fn create_connection_request(vsync_source: i32, event_registration: i32, layer: bool) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER).expect("token");
    data.write_int32(vsync_source);
    data.write_int32(event_registration);
    if layer {
        // A layer handle would be a binder from `createSurface`; nothing in this
        // task has one, so the nullable slot is written null.
        data.write_strong_binder_opt(None);
    } else {
        data.write_strong_binder_opt(None);
    }
    data.set_data_position(0);
    data
}

fn steal_request() -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IDISPLAY_EVENT_CONNECTION)
        .expect("token");
    data.set_data_position(0);
    data
}

#[test]
fn surfaceflinger_resolves_and_answers_the_display_queries_the_image_makes() {
    let mut host = ServiceHost::standard();
    let sf = composer(&mut host);

    // getPhysicalDisplayIds: `long[]` = int32 count + int64 elements.
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER).expect("token");
    data.set_data_position(0);
    let mut reply =
        transact(&mut host, sf, GET_PHYSICAL_DISPLAY_IDS, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let ids = reply.read_int64_array().expect("long[]").expect("non-null");
    assert_eq!(ids, vec![0]);
    assert_eq!(reply.data_avail(), 0);

    // getPhysicalDisplayToken(long) -> IBinder, and it must be routable.
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER).expect("token");
    data.write_int64(0);
    data.set_data_position(0);
    let mut reply =
        transact(&mut host, sf, GET_PHYSICAL_DISPLAY_TOKEN, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let token = reply.read_strong_binder().expect("IBinder").expect("token");
    assert_eq!(
        host.descriptor_of(token),
        Some("android.gui.PhysicalDisplayToken")
    );
    assert_eq!(reply.data_avail(), 0);

    // getDisplayStats(display) -> DisplayStatInfo { long vsyncTime; long
    // vsyncPeriod; }, with vsyncTime on CLOCK_MONOTONIC and vsyncPeriod the same
    // period the vsync thread uses.
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER).expect("token");
    data.write_strong_binder_opt(Some(token));
    data.set_data_position(0);
    let mut reply =
        transact(&mut host, sf, GET_DISPLAY_STATS, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let vsync_time = reply.read_int64().expect("vsyncTime");
    let vsync_period = reply.read_int64().expect("vsyncPeriod");
    assert_eq!(vsync_period, PERIOD_NS_60HZ);
    let now = display_events::now_ns();
    assert!(
        vsync_time <= now && now - vsync_time < 5_000_000_000,
        "vsyncTime {vsync_time} is not a recent CLOCK_MONOTONIC reading (now {now})"
    );
    assert_eq!(reply.data_avail(), 0);

    // getDisplayState(display) -> DisplayState { int layerStack; Rotation
    // orientation; Size layerStackSpaceRect; } (Rotation is @Backing(type="int"),
    // Size is two ints).
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER).expect("token");
    data.write_strong_binder_opt(Some(token));
    data.set_data_position(0);
    let mut reply =
        transact(&mut host, sf, GET_DISPLAY_STATE, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("layerStack"), 0);
    assert_eq!(reply.read_int32().expect("orientation"), 0);
    assert_eq!(reply.read_int32().expect("width"), 1080);
    assert_eq!(reply.read_int32().expect("height"), 2400);
    assert_eq!(reply.data_avail(), 0);

    // getSupportedFrameTimestamps() -> FrameEvent[] (int-backed enum).
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER).expect("token");
    data.set_data_position(0);
    let mut reply = transact(
        &mut host,
        sf,
        GET_SUPPORTED_FRAME_TIMESTAMPS,
        0,
        &mut data,
    )
    .expect("transact");
    no_exception(&mut reply);
    let events = reply
        .read_int32_array()
        .expect("FrameEvent[]")
        .expect("non-null");
    assert_eq!(events, (0..10).collect::<Vec<i32>>());
    assert_eq!(reply.data_avail(), 0);

    // getMaxAcquiredBufferCount() -> int. The image's code is 57 (the r75 tag's
    // declaration order would give 65): a wrong code here is an
    // UNSUPPORTED_ANDROID_API, not a wrong number.
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER).expect("token");
    data.set_data_position(0);
    let mut reply = transact(
        &mut host,
        sf,
        GET_MAX_ACQUIRED_BUFFER_COUNT,
        0,
        &mut data,
    )
    .expect("transact");
    no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("maxAcquiredBuffers"), 2);
    assert_eq!(reply.data_avail(), 0);
}

#[test]
fn the_surface_half_is_a_structured_unsupported_not_a_silent_success() {
    // `createConnection` (ISurfaceComposerClient / `SurfaceSession`) is W-R T1's
    // business, not the clock's. It must be *named*, never answered.
    let mut host = ServiceHost::standard();
    let sf = composer(&mut host);
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER).expect("token");
    data.set_data_position(0);
    let err = transact(&mut host, sf, CREATE_CONNECTION, 0, &mut data)
        .expect_err("createConnection is not implemented by the clock task");
    match err {
        ServiceError::UnsupportedAndroidApi {
            interface,
            code,
            method,
        } => {
            assert_eq!(interface, ISURFACE_COMPOSER);
            assert_eq!(code, CREATE_CONNECTION);
            assert_eq!(method, "createConnection");
        }
        other => panic!("expected UNSUPPORTED_ANDROID_API, got {other:?}"),
    }
}

#[test]
fn create_display_event_connection_answers_a_routable_connection_binder() {
    let mut host = ServiceHost::standard();
    let sf = composer(&mut host);
    let mut data = create_connection_request(
        acos_services::compositor_client::VSYNC_SOURCE_APP,
        0,
        false,
    );
    let mut reply =
        transact(&mut host, sf, CREATE_DISPLAY_EVENT_CONNECTION, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let connection = reply
        .read_strong_binder()
        .expect("IDisplayEventConnection")
        .expect("non-null: the framework treats null as a creation failure");
    assert_eq!(host.descriptor_of(connection), Some(IDISPLAY_EVENT_CONNECTION));
    assert_eq!(reply.data_avail(), 0);

    // The request must not be mis-read: an unknown VsyncSource is a bad request,
    // not a connection.
    let mut data = create_connection_request(7, 0, false);
    let err = transact(&mut host, sf, CREATE_DISPLAY_EVENT_CONNECTION, 0, &mut data)
        .expect_err("unknown VsyncSource");
    assert!(matches!(err, ServiceError::BadRequest { .. }), "got {err:?}");
}

#[test]
fn steal_receive_channel_carries_two_descriptors_and_nothing_else() {
    let mut host = ServiceHost::standard();
    let sf = composer(&mut host);
    let mut data = create_connection_request(
        acos_services::compositor_client::VSYNC_SOURCE_APP,
        0,
        false,
    );
    let mut reply =
        transact(&mut host, sf, CREATE_DISPLAY_EVENT_CONNECTION, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let connection = reply.read_strong_binder().expect("binder").expect("non-null");

    let mut data = steal_request();
    let mut reply =
        transact(&mut host, connection, STEAL_RECEIVE_CHANNEL, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let (receive, send) = display_events::read_bit_tube(&mut reply).expect("two fds");
    assert_ne!(receive, send, "receive and send must be distinct descriptors");
    assert_eq!(reply.data_avail(), 0, "the reply is exactly status + present + 2 fds");
    assert_eq!(reply.owned_file_descriptors().len(), 2);

    // `BitTube::readFromParcel` dups both; a second steal has nothing to give
    // (the stock `writeToParcel` resets the receive end after handing it over).
    let mut data = steal_request();
    let err = transact(&mut host, connection, STEAL_RECEIVE_CHANNEL, 0, &mut data)
        .expect_err("a stolen channel cannot be stolen twice");
    assert!(matches!(err, ServiceError::Internal(_)), "got {err:?}");
}

#[test]
fn latest_vsync_event_data_decodes_in_the_frameworks_read_order() {
    let mut host = ServiceHost::standard();
    let sf = composer(&mut host);
    let mut data = create_connection_request(
        acos_services::compositor_client::VSYNC_SOURCE_APP,
        0,
        false,
    );
    let mut reply =
        transact(&mut host, sf, CREATE_DISPLAY_EVENT_CONNECTION, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let connection = reply.read_strong_binder().expect("binder").expect("non-null");

    let mut data = Parcel::new();
    data.write_interface_token(IDISPLAY_EVENT_CONNECTION)
        .expect("token");
    data.set_data_position(0);
    let mut reply = transact(&mut host, connection, GET_LATEST_VSYNC_EVENT_DATA, 0, &mut data)
        .expect("transact");
    no_exception(&mut reply);
    // `BpDisplayEventConnection::getLatestVsyncEventData` reads the return with
    // `Parcel::readParcelable` (libgui.so 0x161ac5), i.e. present flag first.
    assert_eq!(
        reply.read_int32().expect("present flag"),
        display_events::NON_NULL_PARCELABLE_FLAG,
        "Parcel::readParcelable rejects any flag other than 1 (BAD_TYPE)"
    );
    let frame_interval = reply.read_int64().expect("frameInterval");
    let preferred = reply.read_uint32().expect("preferredFrameTimelineIndex");
    let length = reply.read_uint32().expect("frameTimelinesLength");
    assert_eq!(frame_interval, PERIOD_NS_60HZ);
    assert_eq!(preferred, 0);
    assert_eq!(length, 1);
    assert!(length as usize <= FRAME_TIMELINES_CAPACITY);
    let vsync_id = reply.read_int64().expect("vsyncId");
    let deadline = reply.read_int64().expect("deadlineTimestamp");
    let expected = reply.read_int64().expect("expectedPresentationTime");
    assert!(vsync_id > 0);
    assert_eq!(expected, deadline + frame_interval);
    assert_eq!(reply.data_avail(), 0);
}

/// Regression for the L1 §3.8 NPE: the reply must be exactly
/// `status + present + body`, 4 + 4 + 32 = 40 bytes with one timeline. Both
/// halves of the failure are covered: the flag `readParcelable` requires, and
/// the `ParcelableVsyncEventData::readFromParcel` field order the image reads
/// (`libgui.so` 0x14a550: i64, u32, u32, then 3 x i64 per timeline).
#[test]
fn latest_vsync_event_data_is_the_parcelable_wire_form_byte_for_byte() {
    let mut host = ServiceHost::standard();
    let sf = composer(&mut host);
    let mut data = create_connection_request(
        acos_services::compositor_client::VSYNC_SOURCE_APP,
        0,
        false,
    );
    let mut reply =
        transact(&mut host, sf, CREATE_DISPLAY_EVENT_CONNECTION, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let connection = reply.read_strong_binder().expect("binder").expect("non-null");

    let mut data = Parcel::new();
    data.write_interface_token(IDISPLAY_EVENT_CONNECTION)
        .expect("token");
    data.set_data_position(0);
    let mut reply = transact(&mut host, connection, GET_LATEST_VSYNC_EVENT_DATA, 0, &mut data)
        .expect("transact");

    // Total wire size: exception header (4) + present flag (4) + body (8+4+4+24).
    assert_eq!(reply.data_size(), 48, "status + flag + one timeline");
    no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("flag"), 1);
    let body = reply.data_avail();
    let decoded = display_events::VsyncEventData::read_from_parcel(&mut reply).expect("body");
    assert_eq!(body, 40, "frameInterval+preferred+length+one timeline");
    assert_eq!(decoded.frame_interval, PERIOD_NS_60HZ);
    assert_eq!(decoded.preferred_frame_timeline_index, 0);
    assert_eq!(decoded.frame_timelines_length, 1);
    assert_eq!(decoded.frame_timelines[0].expected_presentation_time,
        decoded.frame_timelines[0].deadline_timestamp + PERIOD_NS_60HZ);
    assert_eq!(reply.data_avail(), 0);

    // The framework's own reader must accept the whole reply (flag + body).
    let mut data = Parcel::new();
    data.write_interface_token(IDISPLAY_EVENT_CONNECTION)
        .expect("token");
    data.set_data_position(0);
    let mut reply = transact(&mut host, connection, GET_LATEST_VSYNC_EVENT_DATA, 0, &mut data)
        .expect("transact");
    no_exception(&mut reply);
    display_events::VsyncEventData::read_parcelable_from(&mut reply)
        .expect("Parcel::readParcelable accepts the reply");
    assert_eq!(reply.data_avail(), 0);
}

/// The load-bearing test: a live connection, `requestNextVsync`, N events off
/// the receive descriptor.
#[test]
fn requested_vsyncs_arrive_at_sixty_hertz_and_only_when_requested() {
    let mut host = ServiceHost::standard();
    let sf = composer(&mut host);
    let mut data = create_connection_request(
        acos_services::compositor_client::VSYNC_SOURCE_APP,
        0,
        false,
    );
    let mut reply =
        transact(&mut host, sf, CREATE_DISPLAY_EVENT_CONNECTION, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let connection = reply.read_strong_binder().expect("binder").expect("non-null");

    let mut data = steal_request();
    let mut reply =
        transact(&mut host, connection, STEAL_RECEIVE_CHANNEL, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let (receive_raw, _send_raw) = display_events::read_bit_tube(&mut reply).expect("two fds");

    // --- no request: no events -------------------------------------------------
    std::thread::sleep(Duration::from_millis(80));
    {
        let receive = reply
            .borrowed_file_descriptor(receive_raw)
            .expect("parcel owns the receive fd");
        assert!(
            display_events::recv_event(receive)
                .expect("recv")
                .is_none(),
            "a connection that was never asked for a vsync must stay silent"
        );
    }

    // --- 5 one-shot requests: 5 events -----------------------------------------
    const N: usize = 5;
    let mut timestamps = Vec::with_capacity(N);
    let mut counts = Vec::with_capacity(N);
    for i in 0..N {
        let mut data = Parcel::new();
        data.write_interface_token(IDISPLAY_EVENT_CONNECTION)
            .expect("token");
        data.set_data_position(0);
        // oneway: the dispatcher writes no reply body.
        let _ = transact(&mut host, connection, REQUEST_NEXT_VSYNC, ONEWAY, &mut data)
            .expect("requestNextVsync");

        let deadline = Instant::now() + Duration::from_millis(400);
        let event = loop {
            {
                let receive = reply
                    .borrowed_file_descriptor(receive_raw)
                    .expect("parcel owns the receive fd");
                if let Some(buf) = display_events::recv_event(receive).expect("recv") {
                    break buf;
                }
            }
            assert!(Instant::now() < deadline, "no vsync {i} within 400 ms");
            std::thread::sleep(Duration::from_millis(2));
        };
        let decoded = decode_event(&event);
        assert_eq!(decoded.kind, display_events::DISPLAY_EVENT_VSYNC);
        assert_eq!(decoded.display_id, 0);
        assert_eq!(decoded.frame_interval, PERIOD_NS_60HZ);
        assert_eq!(decoded.count as usize, i);
        assert_eq!(decoded.first_vsync_id, i as i64 + 1);
        timestamps.push(decoded.timestamp_ns);
        counts.push(decoded.count);
    }

    // Monotonic, ~16.6 ms apart, and on the CLOCK_MONOTONIC grid.
    assert!(timestamps.windows(2).all(|w| w[0] < w[1]), "{timestamps:?}");
    for pair in timestamps.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(
            (PERIOD_NS_60HZ - 1_000_000..=PERIOD_NS_60HZ + 1_000_000).contains(&gap),
            "vsync spacing {gap} ns is not ~16.6 ms ({timestamps:?})"
        );
    }
    let now = display_events::now_ns();
    assert!(
        timestamps.iter().all(|t| *t <= now + 1_000_000),
        "a vsync timestamp is in the future: {timestamps:?} vs now {now}"
    );
    assert_eq!(counts, (0..N as u32).collect::<Vec<_>>());

    // --- and it stops again once the requests are consumed ----------------------
    std::thread::sleep(Duration::from_millis(120));
    let receive = reply
        .borrowed_file_descriptor(receive_raw)
        .expect("parcel owns the receive fd");
    assert!(
        display_events::recv_event(receive).expect("recv").is_none(),
        "the clock must not free-run: no event without a request"
    );
}

#[test]
fn set_vsync_rate_makes_the_clock_continuous_until_it_is_zeroed() {
    let mut host = ServiceHost::standard();
    let sf = composer(&mut host);
    let mut data = create_connection_request(
        acos_services::compositor_client::VSYNC_SOURCE_APP,
        0,
        false,
    );
    let mut reply =
        transact(&mut host, sf, CREATE_DISPLAY_EVENT_CONNECTION, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let connection = reply.read_strong_binder().expect("binder").expect("non-null");

    let mut data = steal_request();
    let mut reply =
        transact(&mut host, connection, STEAL_RECEIVE_CHANNEL, 0, &mut data).expect("transact");
    no_exception(&mut reply);
    let (receive_raw, _) = display_events::read_bit_tube(&mut reply).expect("two fds");

    let mut data = Parcel::new();
    data.write_interface_token(IDISPLAY_EVENT_CONNECTION)
        .expect("token");
    data.write_int32(1);
    data.set_data_position(0);
    let mut rate_reply = transact(&mut host, connection, SET_VSYNC_RATE, 0, &mut data)
        .expect("setVsyncRate(1)");
    no_exception(&mut rate_reply);
    assert_eq!(rate_reply.data_avail(), 0);

    std::thread::sleep(Duration::from_millis(120));
    let mut seen = 0;
    while seen < 3 {
        let receive = reply
            .borrowed_file_descriptor(receive_raw)
            .expect("parcel owns the receive fd");
        if display_events::recv_event(receive).expect("recv").is_some() {
            seen += 1;
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    assert!(seen >= 3, "a non-zero vsync rate must keep the clock running");

    let mut data = Parcel::new();
    data.write_interface_token(IDISPLAY_EVENT_CONNECTION)
        .expect("token");
    data.write_int32(0);
    data.set_data_position(0);
    let mut zero = transact(&mut host, connection, SET_VSYNC_RATE, 0, &mut data)
        .expect("setVsyncRate(0)");
    no_exception(&mut zero);
}

#[test]
fn the_event_struct_is_the_images_216_bytes() {
    // `BitTube::sendObjects(..., objSize)` in the image's
    // `DisplayEventReceiver::sendEvents` passes 0xd8; the encoder must produce
    // exactly that and put every field at the offset the header layout implies.
    assert_eq!(EVENT_SIZE, 0xd8);
    let data = display_events::VsyncEventData::single_timeline(PERIOD_NS_60HZ, 1, 100);
    let event = display_events::encode_vsync_event(42, 100, 0, &data);
    assert_eq!(event.len(), 0xd8);
    let decoded = decode_event(&event);
    assert_eq!(decoded.timestamp_ns, 100);
    assert_eq!(decoded.display_id, 42);
    // The `displayId` field is 8-aligned: bytes 4..8 are the alignment pad.
    assert_eq!(&event[4..8], &[0, 0, 0, 0]);
}

#[test]
fn a_clock_writes_only_when_asked_and_stops_on_drop() {
    // The same contract as the host test, one level down, so a failure points at
    // the clock rather than at the dispatcher.
    let clock = ConnectionClock::new(0, PERIOD_NS_60HZ).expect("clock");
    let mut reply = Parcel::new();
    reply.write_int32(NO_EXCEPTION);
    reply.write_int32(1); // `Parcel::readParcelable`'s non-null flag
    clock.steal_receive_channel(&mut reply).expect("steal");
    reply.set_data_position(4); // past the `binder::Status` header
    let (receive, _send) = display_events::read_bit_tube(&mut reply).expect("two fds");

    std::thread::sleep(Duration::from_millis(60));
    let fd = reply.borrowed_file_descriptor(receive).expect("fd");
    assert!(display_events::recv_event(fd).expect("recv").is_none());
    assert_eq!(clock.events_sent(), 0);

    clock.request_next_vsync();
    let deadline = Instant::now() + Duration::from_millis(400);
    loop {
        let fd = reply.borrowed_file_descriptor(receive).expect("fd");
        if display_events::recv_event(fd).expect("recv").is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "no vsync within 400 ms");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(clock.events_sent(), 1);
    drop(clock);
}

#[test]
fn a_bit_tube_is_a_seqpacket_pair_with_two_distinct_ends() {
    let tube = BitTube::new().expect("socketpair");
    assert!(tube.receive_fd().is_some());
    assert!(tube.send_fd().is_some());
    assert_ne!(
        tube.receive_fd().expect("recv").as_raw_fd(),
        tube.send_fd().expect("send").as_raw_fd()
    );
}

use std::os::fd::AsRawFd;
