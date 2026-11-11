//! The peer acceptance tests: a `Frame` in, `Frame`s out, no socket needed.

use acos_binder_peer::{
    frame_from_parcel, import_parcel, Frame, Peer, F_REQ, F_TXN, JAVA_TAG, NODE_TAG, TF_ONE_WAY,
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
        let binder = reply.read_strong_binder().unwrap().expect("non-null binder");
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
    data.write_interface_token("android.app.IActivityManager").unwrap();
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
    assert_eq!(peer.java_binders().get(&0x77).map(String::as_str), Some("acos.java"));
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
