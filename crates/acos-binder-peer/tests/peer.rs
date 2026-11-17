//! The peer acceptance tests: a `Frame` in, `Frame`s out, no socket needed.

use acos_binder_peer::{
    frame_from_parcel, import_parcel, Frame, Peer, F_HELLO, F_REF, F_REQ, F_RSP, F_TXN, JAVA_TAG,
    NODE_TAG, PROTOCOL_VERSION, REF_RELEASE, TF_ONE_WAY,
};
use acos_parcel::{BinderId, Parcel};
use acos_services::codec;
use acos_services::value::{Parcelable, Value};
use acos_services::ServiceError;

/// Build a request frame the way the driver delivers one: binder arguments a
/// Java process wrote arrive as `BINDER_TYPE_HANDLE(W)`, and the Java id is
/// encoded with `JAVA_TAG` so `frame_from_parcel` produces that wire form.
fn request(target: u64, descriptor: &str, method: &str, args: &[Value], flags: u32) -> Frame {
    let info = acos_aidl::lookup_by_name(descriptor, method)
        .unwrap_or_else(|| panic!("no AIDL entry for {descriptor}::{method}"));
    let parcel = codec::build_request(descriptor, info, args).expect("build request");
    let base = Frame {
        ty: F_REQ,
        seq: 1,
        code: info.code,
        flags,
        target,
        ..Default::default()
    };
    frame_from_parcel(F_REQ, &base, &parcel)
}

/// A Java local binder id as the peer sees it after `import_parcel`.
fn java(wire: u64) -> BinderId {
    BinderId(JAVA_TAG | wire)
}

fn read_reply<'a>(peer: &'a mut Peer, request: Frame) -> (Parcel, Vec<Frame>) {
    let out = peer.handle(request);
    assert!(!out.is_empty(), "peer produced no frames");
    let reply = import_parcel(&out[0]);
    (reply, out[1..].to_vec())
}

fn exception_code(p: &Parcel) -> i32 {
    let mut p = p.clone();
    p.set_data_position(0);
    p.read_int32().unwrap()
}

#[test]
fn service_manager_returns_real_host_nodes_for_window_activity_task_and_package() {
    let mut peer = Peer::new();
    for name in ["window", "activity_task", "package"] {
        let req = request(
            0,
            "android.os.IServiceManager",
            "checkService",
            &[Value::Str(Some(name.into()))],
            0,
        );
        let (mut reply, extra) = read_reply(&mut peer, req);
        assert!(extra.is_empty());
        assert_eq!(exception_code(&reply), 0, "{name}: no exception");
        reply.set_data_position(4);
        let binder = reply
            .read_strong_binder()
            .unwrap()
            .expect("non-null binder");
        let host_id = binder.0;
        assert_eq!(
            peer.host().descriptor_of(BinderId(host_id)),
            Some(match name {
                "window" => "android.view.IWindowManager",
                "activity_task" => "android.app.IActivityTaskManager",
                _ => "android.content.pm.IPackageManager",
            })
        );
    }

    // An unknown name is a real `null`, not an error.
    let req = request(
        0,
        "android.os.IServiceManager",
        "getService",
        &[Value::Str(Some("definitely.not.a.service".into()))],
        0,
    );
    let (mut reply, _) = read_reply(&mut peer, req);
    assert_eq!(exception_code(&reply), 0);
    reply.set_data_position(4);
    assert_eq!(reply.read_strong_binder().unwrap(), None);
}

#[test]
fn iwindowmanager_has_navigation_bar_is_answered_by_the_real_window_service() {
    let mut peer = Peer::new();
    let window = peer
        .host()
        .binder_for_descriptor("android.view.IWindowManager")
        .expect("window service is registered");
    let req = request(
        NODE_TAG | window.0,
        "android.view.IWindowManager",
        "hasNavigationBar",
        &[Value::Int(0)],
        0,
    );
    let (mut reply, extra) = read_reply(&mut peer, req);
    assert!(extra.is_empty());
    assert_eq!(exception_code(&reply), 0);
    reply.set_data_position(4);
    assert_eq!(reply.read_bool().unwrap(), true);
}

#[test]
fn a_service_host_transaction_covered_by_the_existing_tests_round_trips() {
    let mut peer = Peer::new();
    let activity = peer
        .host()
        .binder_for_descriptor("android.app.IActivityManager")
        .expect("activity service");
    let req = request(
        NODE_TAG | activity.0,
        "android.app.IActivityManager",
        "checkPermission",
        &[
            Value::Str(Some("android.permission.INTERNET".into())),
            Value::Int(1000),
            Value::Int(0),
        ],
        0,
    );
    let (mut reply, extra) = read_reply(&mut peer, req);
    assert!(extra.is_empty());
    assert_eq!(exception_code(&reply), 0);
    reply.set_data_position(4);
    assert_eq!(reply.read_int32().unwrap(), 0, "PERMISSION_GRANTED");
}

#[test]
fn unknown_code_is_a_structured_error_and_a_json_line() {
    let path = std::env::temp_dir().join(format!(
        "acos-binder-peer-unsupported-{}-{}.jsonl",
        std::process::id(),
        1
    ));
    let _ = std::fs::remove_file(&path);
    let mut peer = Peer::with_log(&path);
    let activity = peer
        .host()
        .binder_for_descriptor("android.app.IActivityManager")
        .expect("activity service");
    let mut data = Parcel::new();
    data.write_interface_token("android.app.IActivityManager")
        .unwrap();
    let req = Frame {
        ty: F_REQ,
        seq: 9,
        code: 4_000_000,
        flags: 0,
        target: NODE_TAG | activity.0,
        data: data.marshall(),
        offs: vec![],
        fds: vec![],
    };
    let (reply, _) = read_reply(&mut peer, req);
    // `Parcel.EX_UNSUPPORTED_OPERATION`
    assert_eq!(exception_code(&reply), -7);
    assert_eq!(peer.unsupported.len(), 1);
    let record = &peer.unsupported[0];
    assert_eq!(record.descriptor, "android.app.IActivityManager");
    assert_eq!(record.code, 4_000_000);
    assert_eq!(record.error, acos_services::codes::UNSUPPORTED_ANDROID_API);

    let log = std::fs::read_to_string(&path).expect("jsonl log");
    let line = log.lines().next().expect("one line");
    let json: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
    assert_eq!(json["kind"], acos_services::codes::UNSUPPORTED_ANDROID_API);
    assert_eq!(json["descriptor"], "android.app.IActivityManager");
    assert_eq!(json["code"], 4_000_000u64);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn add_service_registers_a_java_local_binder() {
    let mut peer = Peer::new();
    let req = request(
        0,
        "android.os.IServiceManager",
        "addService",
        &[
            Value::Str(Some("acos.java".into())),
            Value::Binder(Some(java(0x77))),
            Value::Bool(false),
            Value::Int(0),
        ],
        0,
    );
    let (reply, _) = read_reply(&mut peer, req);
    assert_eq!(exception_code(&reply), 0);
    assert_eq!(
        peer.java_binders().get(&0x77).map(String::as_str),
        Some("acos.java")
    );
}

#[test]
fn twenty_reverse_deliveries_reach_the_java_receiver_without_a_race() {
    let mut peer = Peer::new();
    let autofill = peer
        .host()
        .binder_for_descriptor("android.view.autofill.IAutoFillManager")
        .expect("autofill service");
    let send_code = acos_aidl::lookup_by_name("com.android.internal.os.IResultReceiver", "send")
        .unwrap()
        .code;
    let receiver_wire = 0xBBB2u64;
    // addClient is oneway; the service answers it by delivering
    // `IResultReceiver.send(0, null)` to the receiver binder in the request.
    for i in 0..20 {
        let req = request(
            NODE_TAG | autofill.0,
            "android.view.autofill.IAutoFillManager",
            "addClient",
            &[
                Value::Binder(Some(java(0xAAA1))),
                Value::TypedObject(Parcelable::Null),
                Value::Int(0),
                Value::Binder(Some(java(receiver_wire))),
            ],
            TF_ONE_WAY,
        );
        let out = peer.handle(req);
        assert_eq!(out.len(), 1, "iteration {i}: expected exactly one delivery");
        let txn = &out[0];
        assert_eq!(txn.ty, F_TXN);
        assert_eq!(txn.target, receiver_wire);
        assert_eq!(txn.code, send_code);
        assert_eq!(transaction_frame_result(txn), 0);
    }
    assert_eq!(peer.unsupported.len(), 0);
}

#[test]
fn reverse_reply_status_codes_are_not_counted_as_deliveries() {
    // A `TF_STATUS_CODE` reply means libbinder failed the transaction before
    // `onTransact` ran (`UNKNOWN_TRANSACTION` from an unrooted node). P2d read
    // "reverse_replies=20" as "20 deliveries" while only one callback ran;
    // this keeps that from being possible again.
    let mut peer = Peer::new();
    let ok = Frame {
        ty: acos_binder_peer::F_REP,
        seq: 1,
        flags: 0,
        ..Default::default()
    };
    let failed = Frame {
        ty: acos_binder_peer::F_REP,
        seq: 2,
        flags: acos_binder_peer::TF_STATUS_CODE,
        ..Default::default()
    };
    assert!(peer.handle(ok).is_empty());
    assert!(peer.handle(failed.clone()).is_empty());
    assert!(peer.handle(failed).is_empty());
    assert_eq!(peer.reverse_replies, 3);
    assert_eq!(peer.reverse_failed, 2);
}

/// Parse a delivery parcel: interface token, then `IResultReceiver.send`'s
/// `(int resultCode, Bundle resultData)`.
fn transaction_frame_result(f: &Frame) -> i32 {
    let mut p = import_parcel(f);
    p.enforce_interface("com.android.internal.os.IResultReceiver")
        .unwrap();
    p.read_int32().unwrap()
}

#[test]
fn host_error_paths_are_not_silent_successes() {
    let mut peer = Peer::new();
    // Unknown descriptor: the host reports a structured error.
    let req = Frame {
        ty: F_REQ,
        seq: 3,
        code: 1,
        flags: 0,
        target: 0,
        data: {
            let mut p = Parcel::new();
            p.write_interface_token("dev.acos.not.AService").unwrap();
            p.marshall()
        },
        offs: vec![],
        fds: vec![],
    };
    let (mut reply, _) = read_reply(&mut peer, req);
    assert_ne!(exception_code(&mut reply), 0);
    assert_eq!(peer.unsupported.len(), 1);

    // A transaction for a target the driver never mapped is also structured.
    let req = Frame::plain(F_REQ, 4, 1, 0, 0x1234, Vec::new());
    let (mut reply, _) = read_reply(&mut peer, req);
    assert_ne!(exception_code(&mut reply), 0);
    assert_eq!(peer.unsupported.len(), 2);
    assert!(matches!(
        reply.read_int32(),
        Ok(c) if c == -7 || c == -3
    ));
}

#[test]
fn error_code_is_the_stable_host_code() {
    // Keep the test honest about which error we expose.
    let err = ServiceError::UnknownBinder { handle: 0x1234 };
    assert_eq!(err.code(), acos_services::codes::SERVICE_UNKNOWN_BINDER);
}

// --- bounded queues (R1) ----------------------------------------------------
//
// The peer's own queues grow with what the guest does. On 2026-11-11 an
// unbounded host-side structure in the ART launcher reached 36 GB anon-rss and
// the kernel OOM killer took the desktop session (results/art-runtime/R1.md).
// These tests pin the ceilings: the peer must refuse work with a structured
// `ART_RESOURCE_LIMIT` and keep its own memory flat, never grow silently.

#[test]
fn the_unsupported_log_is_bounded_and_counts_the_drops() {
    let mut peer = Peer::new();
    let activity = peer
        .host()
        .binder_for_descriptor("android.app.IActivityManager")
        .expect("activity service");
    let mut data = Parcel::new();
    data.write_interface_token("android.app.IActivityManager")
        .unwrap();
    let total = acos_binder_peer::peer::MAX_UNSUPPORTED_RECORDS + 250;
    for _ in 0..total {
        let req = Frame {
            ty: F_REQ,
            seq: 7,
            code: 4_000_001,
            flags: 0,
            target: NODE_TAG | activity.0,
            data: data.marshall(),
            offs: vec![],
            fds: vec![],
        };
        let _ = peer.handle(req);
    }
    assert_eq!(
        peer.unsupported.len(),
        acos_binder_peer::peer::MAX_UNSUPPORTED_RECORDS,
        "the record log must stop at its cap"
    );
    assert_eq!(
        peer.unsupported_dropped(),
        (total - acos_binder_peer::peer::MAX_UNSUPPORTED_RECORDS) as u64,
        "every refused record is counted, never silently dropped"
    );
    assert!(peer.resource_limits() > 0);
}

#[test]
fn the_java_binder_table_is_bounded_with_a_structured_refusal() {
    let mut peer = Peer::new();
    let cap = acos_binder_peer::peer::MAX_JAVA_BINDERS;
    let mut last_exception = 0;
    for i in 0..=cap {
        let req = request(
            0,
            "android.os.IServiceManager",
            "addService",
            &[
                Value::Str(Some(format!("acos.java.{i}"))),
                Value::Binder(Some(java(0x1_0000 + i as u64))),
                Value::Bool(false),
                Value::Int(0),
            ],
            0,
        );
        let (reply, _) = read_reply(&mut peer, req);
        last_exception = exception_code(&reply);
    }
    assert_eq!(
        peer.java_binders().len(),
        cap,
        "the binder table must stop at its cap"
    );
    // `Parcel.EX_UNSUPPORTED_OPERATION`: the guest sees a refusal, not silence.
    assert_eq!(
        last_exception, -7,
        "overflow must be a structured ART_RESOURCE_LIMIT"
    );
    assert!(peer.resource_limits() > 0);
    let last = peer.unsupported.last().expect("an overflow record");
    assert_eq!(last.error, acos_services::codes::ART_RESOURCE_LIMIT);
}

#[test]
fn the_forwarded_transaction_log_is_bounded() {
    let mut peer = Peer::new();
    let package = peer
        .host()
        .binder_for_descriptor("android.content.pm.IPackageManager")
        .expect("package service");
    let cap = acos_binder_peer::peer::MAX_SERVICE_CALLS;
    let total = cap + 500;
    for _ in 0..total {
        let req = request(
            NODE_TAG | package.0,
            "android.content.pm.IPackageManager",
            "getPackagesForUid",
            &[Value::Int(1000)],
            0,
        );
        let _ = peer.handle(req);
    }
    assert!(
        peer.service_calls().len() <= cap,
        "forwarded-transaction log kept {} entries (cap {cap})",
        peer.service_calls().len()
    );
    assert!(peer.resource_limits() >= 500, "the evictions are counted");
}

/// A reply that carries a file descriptor (`SettingsState`'s fd-backed
/// `MemoryIntArray`, a `CursorWindow`'s ashmem) must hand `write_frame` a
/// descriptor that is still open. The reply `Parcel` owns its duplicate and is
/// dropped as soon as the frame exists, so the frame has to own one too —
/// otherwise `sendmsg` fails with `EBADF` and the guest's `transact` blocks
/// forever waiting for a reply that was never sent (P4c §4).
#[test]
fn reply_file_descriptors_stay_open_until_the_frame_is_written() {
    use std::io::{Seek, SeekFrom, Write};

    use acos_binder_peer::close_frame_fds;
    use acos_parcel::ParcelFd;

    let fd = ParcelFd::memfd("acos-binder-peer-frame-fd").expect("memfd_create");
    let mut file = std::fs::File::from(fd.try_clone().expect("dup").into_owned());
    file.write_all(&7i32.to_le_bytes()).expect("write");
    file.seek(SeekFrom::Start(0)).expect("seek");

    let mut parcel = Parcel::new();
    acos_services::exception::write_no_exception(&mut parcel);
    parcel.write_file_descriptor(fd.as_fd()).expect("write fd");
    let frame = frame_from_parcel(
        F_RSP,
        &Frame {
            seq: 9,
            ..Default::default()
        },
        &parcel,
    );
    assert_eq!(
        frame.fds.len(),
        1,
        "the BINDER_TYPE_FD object must move into the frame"
    );
    drop(parcel);

    let mut buf = [0u8; 4];
    let n = unsafe { libc::pread(frame.fds[0], buf.as_mut_ptr().cast(), 4, 0) };
    assert_eq!(n, 4, "the frame's descriptor must outlive the reply parcel");
    assert_eq!(i32::from_le_bytes(buf), 7, "same bytes the provider wrote");
    close_frame_fds(&frame);
}

/// L1 §3.6 regression: libbinder writes a **null** binder as 24 zero bytes plus a
/// 4-byte `Stability::Level` record, and — unlike a real binder — does **not** put
/// it in the offset table (`Parcel::writeObject(obj, /*nullMetaData=*/false)`).
/// Reading it must consume those 28 bytes, or every argument after it shifts by 4
/// (`ViewRootImpl`'s `LayoutParams.token` is exactly that case: the service
/// reported `object at 200 is not a binder`).
#[test]
fn a_null_binder_consumes_its_stability_record_even_without_an_offset_entry() {
    let mut data = Vec::new();
    data.extend_from_slice(&7i32.to_le_bytes());
    // flat_binder_object{type=BINDER, flags=0, binder=0, cookie=0}: libbinder's
    // null-binder record (not all-zero bytes — the type word is set).
    let mut null_obj = [0u8; 24];
    null_obj[0..4].copy_from_slice(&acos_parcel::binder::BINDER_TYPE_BINDER.to_le_bytes());
    data.extend_from_slice(&null_obj);
    data.extend_from_slice(&0i32.to_le_bytes()); // Stability::UNDECLARED
    data.extend_from_slice(&42i32.to_le_bytes());
    let frame = Frame::plain(F_REQ, 1, 1, 0, NODE_TAG | 1, data);
    assert!(
        frame.object_offsets().is_empty(),
        "a null binder is not listed"
    );

    let mut parcel = import_parcel(&frame);
    assert_eq!(parcel.read_int32().expect("int"), 7);
    assert_eq!(parcel.read_strong_binder().expect("binder"), None);
    assert_eq!(
        parcel
            .read_int32()
            .expect("the argument after the null binder"),
        42
    );
    assert_eq!(parcel.data_avail(), 0);
}

/// An imported request must expose its object table.
///
/// The frame carries the table out of band (`offsets`), so `Parcel::unmarshall`
/// alone leaves the imported parcel object-free. A handler that has to find a
/// client binder inside the request — the bluetooth module's
/// `ISynchronousResultReceiver`, which every `IBluetooth` method in the image
/// takes — walks `Parcel::objects()`, and with an empty table it answered "no
/// receiver in the request" while the client waited out its 5 s timeout (observed
/// in the `tail-bluetooth` ART run: two `code 1: no ISynchronousResultReceiver
/// binder in the request (0 object(s), 104 byte(s))` records).
#[test]
fn an_imported_request_exposes_its_object_table() {
    let mut data = Vec::new();
    data.extend_from_slice(&7i32.to_le_bytes());
    let object_at = data.len();
    let mut object = [0u8; 24];
    object[0..4].copy_from_slice(&acos_parcel::binder::BINDER_TYPE_HANDLE.to_le_bytes());
    object[8..16].copy_from_slice(&0x21u64.to_le_bytes());
    data.extend_from_slice(&object);
    data.extend_from_slice(&0i32.to_le_bytes()); // Stability::UNDECLARED
    data.extend_from_slice(&42i32.to_le_bytes());
    let mut frame = Frame::plain(F_REQ, 1, 1, 0, NODE_TAG | 1, data);
    frame.offs = (object_at as u64).to_le_bytes().to_vec();

    let mut parcel = import_parcel(&frame);
    assert_eq!(
        parcel.objects(),
        &[object_at],
        "the importer must register the object-table entry it rewrote"
    );
    let mut probe = parcel.clone();
    probe.set_data_position(parcel.objects()[0]);
    let binder = probe.read_strong_binder().expect("binder").expect("handle");
    assert_eq!(
        binder.0,
        JAVA_TAG | 0x21,
        "a handle imports as a Java binder"
    );
}

/// The same round trip for both kinds of binder: the peer's wire writer must emit
/// libbinder's shape (null binder unlisted, stability 0; real binder listed,
/// stability SYSTEM) and the importer must consume exactly one of each.
#[test]
fn wire_form_round_trips_a_null_binder_and_a_real_one() {
    for id in [BinderId::NULL, BinderId(JAVA_TAG | 5)] {
        let mut written = Parcel::new();
        written.write_int32(7);
        written.write_strong_binder_opt(if id.is_null() { None } else { Some(id) });
        written.write_int32(42);
        let base = Frame::plain(F_REQ, 1, 1, 0, NODE_TAG | 1, Vec::new());
        let frame = frame_from_parcel(F_REQ, &base, &written);
        assert_eq!(
            frame.object_offsets().len(),
            if id.is_null() { 0 } else { 1 },
            "null binders are not in the offset table"
        );

        let mut back = import_parcel(&frame);
        assert_eq!(back.read_int32().expect("int"), 7);
        let want = if id.is_null() { None } else { Some(id) };
        assert_eq!(back.read_strong_binder().expect("binder"), want);
        assert_eq!(back.read_int32().expect("int after the binder"), 42);
        assert_eq!(back.data_avail(), 0);
    }
}

// ---------------------------------------------------------------------------
// ADR-0029 timed module deliveries: the peer sleeps until the deadline, not on
// a fixed poll.
// ---------------------------------------------------------------------------

use std::os::unix::io::{FromRawFd, RawFd};
use std::time::{Duration, Instant};

use acos_binder_peer::{wait_for_idle, IdleWake};
use acos_services::module::{ModuleConfig, ModuleRegistry, Profile};
use acos_services::ServiceHost;

/// A pair of connected Unix sockets: `(guest_side, peer_side)`. The guest side
/// is kept alive and silent, exactly like an idle guest.
fn silent_socketpair() -> (RawFd, RawFd) {
    let mut fds = [0 as RawFd; 2];
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "socketpair: {}", std::io::Error::last_os_error());
    (fds[0], fds[1])
}

/// A plain pipe: `(read, write)`.
fn wakeup_pipe() -> (RawFd, RawFd) {
    let mut fds = [0 as RawFd; 2];
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe: {}", std::io::Error::last_os_error());
    (fds[0], fds[1])
}

fn wake(write: RawFd) {
    let byte = 1u8;
    let rc = unsafe { libc::write(write, (&byte as *const u8).cast::<libc::c_void>(), 1) };
    assert_eq!(rc, 1);
}

/// A peer with `core` attached and nothing else.
fn core_only_peer() -> Peer {
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    Peer::with_host(host, None)
}

#[test]
fn an_idle_peer_with_no_timed_module_has_no_tick_wakeups() {
    let mut peer = core_only_peer();
    assert!(
        peer.next_timed_deadline().is_none(),
        "no attached module schedules a delivery, so there is nothing to wake for"
    );
    let (guest, _silent_guest) = silent_socketpair();
    let (wake_read, wake_write) = wakeup_pipe();
    // The stop wake arrives after two full seconds: the whole time the loop must
    // sleep in one unbounded `poll(2)`, not tick.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        wake(wake_write);
    });
    let started = Instant::now();
    let mut ticks = 0usize;
    let mut waits = 0usize;
    loop {
        let deadline = peer
            .next_timed_deadline()
            .map(|due| due.saturating_duration_since(Instant::now()));
        match wait_for_idle(guest, wake_read, deadline).expect("poll") {
            IdleWake::Deadline => {
                ticks += 1;
                let _ = peer.tick();
            }
            IdleWake::Frame | IdleWake::Wake => break,
        }
        waits += 1;
        assert!(
            waits < 8,
            "the peer must not poll: {waits} wait(s) in {ticks} tick(s)"
        );
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1900),
        "the peer slept until the stop wake, got {elapsed:?}"
    );
    assert_eq!(
        ticks, 0,
        "an idle session with no timed module must not tick"
    );
    assert_eq!(waits, 0, "one unbounded wait, no poll loop");
    unsafe {
        libc::close(guest);
        libc::close(_silent_guest);
        libc::close(wake_read);
    }
}

#[test]
fn a_scheduled_fix_fires_within_five_ms_of_its_deadline() {
    const DESCRIPTOR: &str = "android.location.ILocationManager";
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    registry
        .attach(
            &mut host,
            "location",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("location attaches");
    let mut peer = Peer::with_host(host, None);

    // Register a Java listener exactly as `requestLocationUpdates` does, so the
    // delivered fix becomes a `system -> app` frame with a driver handle.
    let binder = peer
        .host_mut()
        .check_service("location")
        .expect("lookup")
        .expect("location is served");
    let listener = 0x2233u64;
    let mut data = Parcel::new();
    data.write_interface_token(DESCRIPTOR).expect("token");
    data.write_string16("gps").expect("provider");
    data.write_int32(1);
    data.write_string16("gps").expect("request provider");
    data.write_int64(1000);
    data.write_int32(0);
    data.write_int64(0);
    data.write_int64(0);
    data.write_int32(i32::MAX);
    data.write_int64(0);
    data.write_float(0.0);
    data.write_int64(0);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(0);
    data.write_strong_binder(java(listener));
    data.write_string16("dev.acos.fixtures.locationprobe")
        .expect("package");
    data.write_string16_opt(None).expect("tag");
    data.write_string16("listener").expect("listener id");
    data.set_data_position(0);
    let mut reply = Parcel::new();
    peer.host_mut()
        .transact(binder, 3, &mut data, &mut reply, 0)
        .expect("registerLocationListener");
    // The registration itself notifies the listener; drop that first delivery so
    // the measurement is about the track point.
    peer.tick();

    registry
        .control(
            peer.host_mut(),
            "location",
            "play_track",
            &serde_json::json!({
                "points": [{"lat": 37.1, "lng": -122.1}, {"lat": 37.2, "lng": -122.2}],
                "interval_ms": 40
            }),
        )
        .expect("play_track");
    let deadline = peer
        .next_timed_deadline()
        .expect("a playing track schedules the next point");

    let (guest, _silent_guest) = silent_socketpair();
    let (wake_read, _wake_write) = wakeup_pipe();
    let late = loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        match wait_for_idle(guest, wake_read, Some(wait)).expect("poll") {
            IdleWake::Deadline => break Instant::now().saturating_duration_since(deadline),
            other => panic!("expected the track deadline, got {other:?}"),
        }
    };
    let frames: Vec<acos_binder_peer::Frame> = peer.tick().into_iter().map(|(_, f)| f).collect();
    assert!(
        !frames.is_empty(),
        "the track point must reach the guest as a frame at its deadline"
    );
    assert_eq!(frames[0].ty, F_TXN);
    assert_eq!(frames[0].target, listener);
    assert!(
        late <= Duration::from_millis(5),
        "the fix fired {late:?} after its deadline (max 5 ms)"
    );
    assert_eq!(
        peer.next_timed_deadline(),
        Some(deadline + Duration::from_millis(40)),
        "the schedule moves one interval per point, not by the time that elapsed"
    );
    unsafe {
        libc::close(guest);
        libc::close(_silent_guest);
        libc::close(wake_read);
        libc::close(_wake_write);
    }
}

/// The reference model, host side: a registration is a strong reference on a
/// guest binder, and dropping the last one is a message to the driver (`F_REF`
/// release) -- never a silent leak of a node that nobody references any more.
#[test]
fn a_dropped_registration_releases_the_guest_node_over_the_wire() {
    let mut peer = Peer::new();
    // The handshake decides whether the driver can be told at all.
    let out = peer.handle(Frame::plain(F_HELLO, PROTOCOL_VERSION, 0, 0, 0, Vec::new()));
    assert!(out.is_empty(), "hello is answered with nothing");
    assert_eq!(peer.driver_protocol(), Some(PROTOCOL_VERSION));

    let id = java(0x99);
    peer.host_mut().binder_registry().register_at(
        id.0,
        "test.listener",
        acos_parcel::BinderKind::Local,
    );
    assert_eq!(
        peer.retain_binder(id),
        1,
        "a registration takes one reference"
    );
    assert_eq!(peer.retain_binder(id), 2);
    assert_eq!(peer.release_binder(id), 1);
    // Still referenced: nothing to tell the driver.
    let ping = Frame::plain(
        F_REQ,
        7,
        acos_binder_peer::PING_TRANSACTION,
        0,
        0,
        Vec::new(),
    );
    let out = peer.handle(ping.clone());
    assert!(
        !out.iter().any(|f| f.ty == F_REF),
        "a binder that is still registered must not be released"
    );

    assert_eq!(peer.release_binder(id), 0);
    let out = peer.handle(ping);
    let refs: Vec<&Frame> = out.iter().filter(|f| f.ty == F_REF).collect();
    assert_eq!(
        refs.len(),
        1,
        "the last reference release is one F_REF frame"
    );
    assert_eq!(refs[0].code, REF_RELEASE);
    assert_eq!(refs[0].target, 0x99, "the driver sees its own wire handle");
    assert_eq!(peer.held_refs_of(id), 0);
}

/// Frame order inside one turn: a delivery is written **before** the release of
/// the reference that made the delivery possible. The driver unroots a node on
/// `BR_RELEASE`/`BR_DECREFS`, so a release that came first would turn a callback
/// that was queued while its registration was still live into `DEAD_OBJECT`.
#[test]
fn a_delivery_is_written_before_the_reference_release_of_the_same_turn() {
    let mut peer = Peer::new();
    let out = peer.handle(Frame::plain(F_HELLO, PROTOCOL_VERSION, 0, 0, 0, Vec::new()));
    assert!(out.is_empty());
    let id = java(0x55);
    peer.host_mut().binder_registry().register_at(
        id.0,
        "test.listener",
        acos_parcel::BinderKind::Local,
    );
    assert_eq!(peer.retain_binder(id), 1);

    let mut data = Parcel::new();
    data.write_interface_token("test.listener").expect("token");
    peer.host_mut()
        .queue_delivery(acos_services::host::PendingDelivery {
            target: id,
            interface: "test.listener".into(),
            code: 1,
            method: "onEvent".into(),
            data,
        });
    // The registration goes away in the same turn the delivery was queued.
    assert_eq!(peer.release_binder(id), 0);

    let ping = Frame::plain(
        F_REQ,
        11,
        acos_binder_peer::PING_TRANSACTION,
        0,
        0,
        Vec::new(),
    );
    let out = peer.handle(ping);
    let kinds: Vec<u32> = out.iter().map(|f| f.ty).collect();
    let delivery = kinds
        .iter()
        .position(|ty| *ty == F_TXN)
        .expect("the queued delivery is in this batch");
    let release = kinds
        .iter()
        .position(|ty| *ty == F_REF)
        .expect("the released reference is in this batch");
    assert!(
        delivery < release,
        "delivery frames must precede release frames, got {kinds:?}"
    );
}

/// An older driver (no `F_HELLO`) must never receive `F_REF`: the frame would
/// be answered with "unexpected frame type" and the release would look like a
/// driver bug. The peer keeps the node alive for the session instead, and counts
/// what it suppressed.
#[test]
fn a_driver_without_the_handshake_never_gets_a_reference_frame() {
    let mut peer = Peer::new();
    assert_eq!(peer.driver_protocol(), None);
    let id = java(0x42);
    peer.host_mut().binder_registry().register_at(
        id.0,
        "test.listener",
        acos_parcel::BinderKind::Local,
    );
    assert_eq!(peer.retain_binder(id), 1);
    assert_eq!(peer.release_binder(id), 0);
    let ping = Frame::plain(
        F_REQ,
        9,
        acos_binder_peer::PING_TRANSACTION,
        0,
        0,
        Vec::new(),
    );
    let out = peer.handle(ping);
    assert!(
        !out.iter().any(|f| f.ty == F_REF),
        "an old driver must not be sent a frame it does not know"
    );
    assert_eq!(peer.ref_frames_suppressed(), 1);
}
