//! MPROC2 step 3a: N tagged guest connections, one per app process.
//!
//! The invariant these tests pin: a driver wire handle is only unique *inside
//! its own process*, so two processes must never be addressable through the same
//! [`BinderId`], and a delivery must reach the process that owns the target
//! binder — never another one that happens to use the same wire number.

use acos_binder_peer::{
    import_parcel_for, java_binder_id, owner_process, peer::MAX_JAVA_BINDERS, Frame, Peer,
    ProcessConn, TagRefusal, F_REQ, F_TXN, JAVA_TAG, NODE_TAG, TAG_PRIMARY,
};
use acos_parcel::Parcel;
use acos_services::codec;
use acos_services::value::Value;

/// Build a request frame the way process `tag`'s driver delivers one.
/// `tag` documents which process builds the frame; the binder ids in `args`
/// carry it (`java_binder_id`).
fn request_for(_tag: u32, target: u64, descriptor: &str, method: &str, args: &[Value], flags: u32) -> Frame {
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
    acos_binder_peer::frame_from_parcel(F_REQ, &base, &parcel)
}

fn exception_code(p: &Parcel) -> i32 {
    let mut p = p.clone();
    p.set_data_position(0);
    p.read_int32().unwrap()
}

/// `addService(name, binder, isolated, priority)` from process `tag`.
fn add_service(peer: &mut Peer, tag: u32, wire: u32, name: &str) -> i32 {
    let req = request_for(
        tag,
        0,
        "android.os.IServiceManager",
        "addService",
        &[
            Value::Str(Some(name.to_string())),
            Value::Binder(Some(java_binder_id(tag, wire))),
            Value::Bool(false),
            Value::Int(0),
        ],
        0,
    );
    let out = peer.handle_tagged(tag, req);
    assert_eq!(out.len(), 1, "addService must be answered");
    assert_eq!(out[0].0, tag, "the reply goes back on the caller's connection");
    exception_code(&import_parcel_for(tag, &out[0].1))
}

#[test]
fn a_handle_is_scoped_to_its_process() {
    // Wire handle 7 exists in *both* processes (each driver numbers from 1), and
    // they are different binders registered under different names.
    let mut peer = Peer::new();
    peer.register_process(1, "dev.acos.fixtures.proc-a").unwrap();
    peer.register_process(2, "dev.acos.fixtures.proc-b").unwrap();
    assert_eq!(peer.tags(), vec![0, 1, 2]);

    assert_eq!(add_service(&mut peer, 1, 7, "acos.a"), 0);
    assert_eq!(add_service(&mut peer, 2, 7, "acos.b"), 0);

    assert_eq!(peer.java_binders_for(1).get(&7).map(String::as_str), Some("acos.a"));
    assert_eq!(peer.java_binders_for(2).get(&7).map(String::as_str), Some("acos.b"));
    assert_eq!(
        peer.all_java_binders(),
        vec![
            (1, 7, "acos.a".to_string()),
            (2, 7, "acos.b".to_string())
        ]
    );
    // The primary process never saw either of them.
    assert!(peer.java_binders().is_empty());

    // The ids differ, and each id names its owner.
    assert_ne!(java_binder_id(1, 7), java_binder_id(2, 7));
    assert_eq!(owner_process(java_binder_id(1, 7)), Some(1));
    assert_eq!(owner_process(java_binder_id(2, 7)), Some(2));
    assert_eq!(owner_process(java_binder_id(TAG_PRIMARY, 7)), Some(0));
    // A host node is not owned by any app process.
    assert_eq!(owner_process(acos_parcel::BinderId(NODE_TAG | 5)), None);
}

#[test]
fn the_service_host_records_the_owner_of_every_app_binder() {
    let mut peer = Peer::new();
    peer.register_process(1, "a").unwrap();
    peer.register_process(2, "b").unwrap();
    assert_eq!(add_service(&mut peer, 1, 0x11, "acos.a"), 0);
    assert_eq!(add_service(&mut peer, 2, 0x11, "acos.b"), 0);

    let host = peer.host();
    assert_eq!(host.binder_owner(java_binder_id(1, 0x11)), Some(1));
    assert_eq!(host.binder_owner(java_binder_id(2, 0x11)), Some(2));
    assert_eq!(
        host.binders_of_process(2),
        vec![java_binder_id(2, 0x11)],
        "process 2 owns exactly its own binder"
    );
}

#[test]
fn a_parcel_imported_from_two_processes_never_collides() {
    let base = Frame {
        ty: F_REQ,
        seq: 3,
        code: 1,
        flags: 0,
        target: 0,
        ..Default::default()
    };
    let mut parcel = Parcel::new();
    parcel.write_strong_binder(java_binder_id(1, 7));
    let frame = acos_binder_peer::frame_from_parcel(F_REQ, &base, &parcel);

    let from_one = import_parcel_for(1, &frame).binder_ids();
    let from_two = import_parcel_for(2, &frame).binder_ids();
    assert_eq!(from_one, vec![java_binder_id(1, 7)]);
    assert_eq!(from_two, vec![java_binder_id(2, 7)]);
    assert_ne!(from_one, from_two);
    // The legacy entry point is the primary process.
    assert_eq!(
        acos_binder_peer::import_parcel(&frame).binder_ids(),
        vec![java_binder_id(TAG_PRIMARY, 7)]
    );
    // Whole-parcel equivalence: the tag lives in the object id, not the payload.
    assert_eq!(
        from_one[0].0 & 0xffff_ffff,
        from_two[0].0 & 0xffff_ffff,
        "the wire handle itself is the same number in both processes"
    );
    assert_eq!(from_one[0].0 & JAVA_TAG, JAVA_TAG);
}

/// Queue an `IResultReceiver.send` delivery to `target` the way a session
/// control op does (outside any handler call).
fn queue_result_delivery(peer: &mut Peer, target: acos_parcel::BinderId, result_code: i32) {
    let info = acos_aidl::lookup_by_name("com.android.internal.os.IResultReceiver", "send")
        .expect("IResultReceiver.send is in the AIDL table");
    let args = vec![Value::Int(result_code), Value::Bundle(None)];
    let data =
        codec::build_request("com.android.internal.os.IResultReceiver", info, &args).unwrap();
    peer.host_mut().queue_delivery(acos_services::PendingDelivery {
        target,
        interface: "com.android.internal.os.IResultReceiver".into(),
        code: info.code,
        method: info.name.clone(),
        data,
    });
}

#[test]
fn a_delivery_reaches_the_process_that_owns_the_target_binder() {
    // Both processes use wire handle 0xBBB2 for a different binder; the delivery
    // names process 2's, and the primary process must not see it (that is how a
    // second process's `IApplicationThread` used to be reachable by accident).
    let mut peer = Peer::new();
    peer.register_process(1, "a").unwrap();
    peer.register_process(2, "b").unwrap();
    queue_result_delivery(&mut peer, java_binder_id(2, 0xBBB2), 0);

    let out = peer.handle_tagged(1, Frame::default());
    assert_eq!(out.len(), 1, "exactly one delivery");
    let (dest, txn) = &out[0];
    assert_eq!(*dest, 2, "the delivery belongs to process 2's connection");
    assert_eq!(txn.ty, F_TXN);
    assert_eq!(txn.target, 0xBBB2, "process 2's wire handle");
    assert_eq!(peer.unsupported.len(), 0);

    // The same delivery shape owned by process 1 goes to process 1.
    queue_result_delivery(&mut peer, java_binder_id(1, 0xBBB2), 0);
    let out = peer.handle_tagged(2, Frame::default());
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, 1);
}

#[test]
fn a_delivery_to_a_dead_process_is_a_structured_record_not_a_silent_drop() {
    let mut peer = Peer::new();
    peer.register_process(1, "a").unwrap();
    peer.register_process(2, "b").unwrap();
    queue_result_delivery(&mut peer, java_binder_id(2, 0xBBB2), 0);
    peer.disconnect_process(2);

    let out = peer.handle_tagged(1, Frame::default());
    assert!(
        out.is_empty(),
        "nothing can be written to a closed connection"
    );
    let record = peer.unsupported.last().expect("the drop must be recorded");
    assert!(
        record.message.contains("has no connection"),
        "record: {}",
        record.message
    );
}

#[test]
fn a_connection_without_a_handshake_is_the_primary_process() {
    // The pre-MPROC2 driver sends no hello. That must stay the primary process,
    // exactly as before, or an older guest would be silently mis-addressed.
    let mut peer = Peer::new();
    let req = request_for(
        TAG_PRIMARY,
        0,
        "android.os.IServiceManager",
        "addService",
        &[
            Value::Str(Some("acos.java".into())),
            Value::Binder(Some(java_binder_id(TAG_PRIMARY, 0x77))),
            Value::Bool(false),
            Value::Int(0),
        ],
        0,
    );
    let out = peer.handle(req);
    assert_eq!(out.len(), 1);
    assert_eq!(peer.java_binders().get(&0x77).map(String::as_str), Some("acos.java"));
    assert_eq!(peer.host().binder_owner(java_binder_id(0, 0x77)), Some(TAG_PRIMARY));
}

#[test]
fn a_duplicate_tag_is_refused_and_the_first_connection_keeps_it() {
    let mut peer = Peer::new();
    assert!(peer.register_process(4, "a").is_ok());
    assert_eq!(
        peer.register_process(4, "b"),
        Err(TagRefusal::AlreadyConnected(4))
    );
    assert_eq!(peer.process_name(4), Some("a"), "the first name is kept");
    // Re-announcing the primary process (a driver reconnect) is not an error.
    assert!(peer.register_process(0, "primary-again").is_ok());
    assert_eq!(peer.process_name(0), Some("primary-again"));
}

#[test]
fn an_out_of_range_or_full_process_table_is_refused() {
    let mut peer = Peer::new();
    assert_eq!(
        peer.register_process(64, "too-high"),
        Err(TagRefusal::OutOfRange(64))
    );
    assert_eq!(acos_binder_peer::MAX_PROCESS_TAG, 63);
    // Distinct reasons, distinct stable codes: never one generic failure.
    assert_eq!(TagRefusal::OutOfRange(9).code(), "PROCESS_TAG_OUT_OF_RANGE");
    assert_eq!(TagRefusal::AlreadyConnected(1).code(), "PROCESS_TAG_IN_USE");
    assert_eq!(TagRefusal::TooManyProcesses(64).code(), "PROCESS_TABLE_FULL");
}

#[test]
fn a_disconnected_process_drops_only_its_own_binders() {
    let mut peer = Peer::new();
    peer.register_process(1, "a").unwrap();
    peer.register_process(2, "b").unwrap();
    assert_eq!(add_service(&mut peer, 1, 7, "acos.a"), 0);
    assert_eq!(add_service(&mut peer, 2, 7, "acos.b"), 0);

    let (binders, owners) = peer.disconnect_process(2);
    assert_eq!(binders, 1);
    assert_eq!(owners, 1);
    assert_eq!(peer.tags(), vec![0, 1], "process 2 is gone");
    assert!(peer.java_binders_for(2).is_empty());
    assert_eq!(
        peer.java_binders_for(1).get(&7).map(String::as_str),
        Some("acos.a"),
        "process 1 is untouched"
    );
    assert_eq!(peer.host().binder_owner(java_binder_id(2, 7)), None);
    assert_eq!(peer.host().binder_owner(java_binder_id(1, 7)), Some(1));
}

#[test]
fn the_java_binder_cap_is_per_process() {
    // One process filling its table must not deny another process its own.
    let mut peer = Peer::new();
    peer.register_process(1, "a").unwrap();
    peer.register_process(2, "b").unwrap();
    for i in 0..MAX_JAVA_BINDERS {
        assert_eq!(add_service(&mut peer, 1, 0x1_0000 + i as u32, "filled"), 0);
    }
    assert_eq!(peer.java_binders_for(1).len(), MAX_JAVA_BINDERS);
    assert_eq!(add_service(&mut peer, 2, 1, "fresh"), 0);
    assert_eq!(peer.java_binders_for(2).len(), 1);
    // And the full process still refuses with a structured error.
    assert_eq!(
        add_service(&mut peer, 1, 0xffff_fffe, "overflow"),
        -7,
        "EX_UNSUPPORTED_OPERATION, i.e. a structured ART_RESOURCE_LIMIT"
    );
    assert!(peer.resource_limits() > 0);
}

#[test]
fn the_serve_loop_accepts_two_tagged_connections_and_answers_each() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // `AF_UNIX` paths are 108 bytes: keep the socket under the worktree's
    // scratch dir (never /tmp), with a short name.
    // `AF_UNIX` paths are 108 bytes: resolve the `..`s so the joined path is
    // the short one (worktree scratch, never /tmp).
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.cache/mproc2-socks");
    std::fs::create_dir_all(dir).expect("socket dir");
    let dir = std::fs::canonicalize(dir).expect("socket dir");
    let path = format!("{}/s{}.sock", dir.display(), std::process::id());
    let _ = std::fs::remove_file(&path);
    let listener = acos_binder_peer::listen(&path).expect("listen");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let server = std::thread::spawn(move || {
        let mut peer = Peer::new();
        let outcome = acos_binder_peer::serve::serve(&mut peer, listener, &stop_thread)
            .expect("serve");
        unsafe { libc::close(listener) };
        (outcome, peer.tags(), peer.all_java_binders())
    });

    // Two clients, each naming its process, each getting its own answer. The
    // transport is `SOCK_SEQPACKET`, like the driver's, so the test must open
    // that kind of socket (a `UnixStream` is SOCK_STREAM and cannot connect).
    let mut clients = Vec::new();
    for tag in 1..=2u32 {
        let fd = connect_seqpacket(&path);
        // The connection owns the fd again once `read_frame`/`write_frame` use it.
        acos_binder_peer::write_frame(fd, &Frame::hello(tag, &format!("proc-{tag}"))).unwrap();
        acos_binder_peer::write_frame(
            fd,
            &Frame::plain(
                acos_binder_peer::F_REQ,
                100 + tag,
                acos_binder_peer::PING_TRANSACTION,
                0,
                0,
                Vec::new(),
            ),
        )
        .unwrap();
        clients.push((tag, fd));
    }
    for (tag, fd) in &clients {
        let reply = acos_binder_peer::read_frame(*fd).expect("reply");
        assert_eq!(reply.ty, acos_binder_peer::F_RSP, "process {tag} got its reply");
        assert_eq!(reply.seq, 100 + tag, "the reply is correlated to its own request");
    }
    for (_, fd) in &clients {
        unsafe { libc::close(*fd) };
    }
    stop.store(true, Ordering::Release);
    let (outcome, tags, binders) = server.join().expect("serve thread");
    assert_eq!(outcome.accepted, 2, "both handshakes were accepted");
    assert_eq!(outcome.peak_connections, 2);
    assert_eq!(outcome.closed, 2);
    assert_eq!(tags, vec![0], "both secondary processes closed their connections");
    assert!(binders.is_empty());
    let _ = std::fs::remove_file(&path);
}

/// A connected `SOCK_SEQPACKET` client socket (what the driver uses).
fn connect_seqpacket(path: &str) -> std::os::fd::RawFd {
    use std::os::fd::RawFd;
    unsafe {
        let fd: RawFd = libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0);
        assert!(fd >= 0, "socket: {}", std::io::Error::last_os_error());
        let mut sa: libc::sockaddr_un = std::mem::zeroed();
        sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (i, b) in path.as_bytes().iter().enumerate() {
            sa.sun_path[i] = *b as libc::c_char;
        }
        let len = (std::mem::size_of::<libc::sa_family_t>() + path.len() + 1) as libc::socklen_t;
        let rc = libc::connect(fd, &sa as *const _ as *const libc::sockaddr, len);
        assert_eq!(rc, 0, "connect {path}: {}", std::io::Error::last_os_error());
        fd
    }
}

/// MPROC7: the launch facts are evidence, not connection state.
///
/// The launcher classifies a run *after* the guest has ended, from
/// `PeerSummary::{bind_bytes, delivered, txn_bytes, txn_delivered}`. MPROC2
/// moved those facts into the per-connection `LaunchState` and wiped them in
/// `disconnect_process`, so an ordinary launch whose guest exits right after
/// `bindApplication` (the `until=application-created` watcher quits the looper)
/// reported `application_created=false` / `GUEST_EXITED` even though the bind
/// had been delivered. `launch_hang::ordinary_launch_to_application_created_stays_fast`
/// failed on `mproc5b`/`mproc5c` for exactly that reason and passes again with
/// the facts kept.
#[test]
fn launch_delivery_facts_survive_the_guest_connection() {
    let mut peer = Peer::new();
    // The primary process: `launch_bind_len`/`launch_txn_len` are that
    // process's launch facts (the secondary processes' first launch moves onto
    // the host encoder, MPROC7).
    peer.register_process(TAG_PRIMARY, "dev.acos.fixtures.proc-a").unwrap();

    // The guest pushes the two marshalled requests the launch protocol carries
    // (`bindApplication`, then the one `ClientTransaction`).
    push_launch(
        &mut peer,
        TAG_PRIMARY,
        acos_binder_peer::peer::LAUNCH_PUSH_BIND,
        b"bind-request-bytes",
    );
    push_launch(
        &mut peer,
        TAG_PRIMARY,
        acos_binder_peer::peer::LAUNCH_PUSH_TRANSACTION,
        b"client-transaction-bytes",
    );
    assert_eq!(peer.launch_bind_len(), Some(18));
    assert_eq!(peer.launch_txn_len(), Some(24));

    peer.disconnect_process(TAG_PRIMARY);
    assert!(
        !peer.process(TAG_PRIMARY).is_some_and(ProcessConn::connected),
        "the process is gone"
    );
    // The record of what the host delivered is still readable: it is what the
    // launcher reports for a guest that ended before the launcher stopped it.
    assert_eq!(
        peer.launch_bind_len(),
        Some(18),
        "the delivered bind request is evidence after the guest died"
    );
    assert_eq!(
        peer.launch_txn_len(),
        Some(24),
        "the delivered launch transaction is evidence after the guest died"
    );

    // A new guest under the same tag starts with a clean launch state: nothing
    // from the previous instance may be replayed into it.
    peer.register_process(TAG_PRIMARY, "dev.acos.fixtures.proc-a").unwrap();
    assert_eq!(peer.launch_bind_len(), None);
    assert_eq!(peer.launch_txn_len(), None);
}

/// Push one `acos.launch` request frame from process `tag` (the guest's
/// `pushToPeer` in `LaunchMain.java`).
fn push_launch(peer: &mut Peer, tag: u32, code: u32, bytes: &[u8]) {
    let mut parcel = Parcel::new();
    parcel
        .write_interface_token("dev.acos.art.ILaunch")
        .expect("interface token");
    parcel
        .write_byte_array(Some(bytes))
        .expect("byte array");
    let base = Frame {
        ty: F_REQ,
        seq: 7,
        code,
        flags: 0,
        target: NODE_TAG | acos_binder_peer::peer::LAUNCH_NODE,
        ..Default::default()
    };
    let frame = acos_binder_peer::frame_from_parcel(F_REQ, &base, &parcel);
    let out = peer.handle_tagged(tag, frame);
    assert_eq!(out.len(), 1, "the push is answered on the caller's connection");
    assert_eq!(out[0].0, tag);
    let mut reply = import_parcel_for(tag, &out[0].1);
    let code = reply.read_int32().expect("reply status");
    if code != 0 {
        let msg = reply.read_string16().ok().flatten();
        panic!("launch push refused: code={code} msg={msg:?}");
    }
}
