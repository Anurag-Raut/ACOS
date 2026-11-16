//! MPROC3 step 3c: a binder that lives in another app process.
//!
//! The invariant these tests pin: a process never receives another process's
//! driver handle. It receives a *node* (a synthetic peer node), a transaction on
//! that node is forwarded to the process that owns the binder, the owner's reply
//! comes back to the caller with the caller's own sequence number, the owner's
//! death notifies every holder, and a holder that releases the node frees it once
//! its owner is gone.

use acos_binder_peer::{
    import_parcel_for, java_binder_id, owner_process, Frame, Peer, FOREIGN_FLAG, F_REQ, F_RSP,
    F_TXN, JAVA_TAG, PING_TRANSACTION, TAG_PRIMARY,
};
use acos_parcel::{BinderId, Parcel};

/// A host reply the peer has to route: it carries process `1`'s binder (wire 7)
/// and is destined for `dest`. Returns the frame plus the node the receiver gets.
fn app_binder_reply(peer: &mut Peer, dest: u32) -> (Frame, u64) {
    // The peer has to know the object exists and who owns it; a real session
    // learns that from the process's own `addService`/parcel.
    peer.host_mut().set_binder_owner(java_binder_id(1, 7), 1);
    let mut parcel = Parcel::new();
    parcel.write_strong_binder(java_binder_id(1, 7));
    let exported = peer.export_for_process(dest, &parcel);
    let node = exported
        .binder_ids()
        .into_iter()
        .map(|id| id.0)
        .find(|id| id & FOREIGN_FLAG == FOREIGN_FLAG)
        .expect("the reply carries a foreign node");
    let frame = acos_binder_peer::frame_from_parcel(
        F_RSP,
        &Frame::plain(F_RSP, 7, 0, 0, 0, Vec::new()),
        &exported,
    );
    (frame, node)
}

fn peer_with_a_service() -> (Peer, Frame, u64) {
    let mut peer = Peer::new();
    peer.register_process(1, "dev.acos.fixtures.proc-b").unwrap();
    let (reply, node) = app_binder_reply(&mut peer, TAG_PRIMARY);
    (peer, reply, node)
}

/// The binder id a reply frame carries, as the receiving driver would import it.
fn reply_binder(tag: u32, frame: &Frame) -> BinderId {
    import_parcel_for(tag, frame)
        .binder_ids()
        .into_iter()
        .next()
        .expect("the reply carries a binder")
}

#[test]
fn a_binder_of_another_process_reaches_the_caller_as_a_node() {
    let (peer, reply, node) = peer_with_a_service();
    assert_eq!(peer.foreign_owner(node), Some((1, 7)));
    assert_eq!(
        reply_binder(TAG_PRIMARY, &reply),
        BinderId(node),
        "the caller imports the node it was handed"
    );
    assert_eq!(
        peer.foreign_nodes(),
        vec![(node, 1, 7, vec![TAG_PRIMARY])],
        "the node names its owner and the process that holds it"
    );
    // The reply frame is a *node* on the wire (BINDER_TYPE_BINDER): the driver
    // maps it to a local handle, never to wire handle 7 of another process.
    assert_eq!(
        peer.host().binder_owner(java_binder_id(1, 7)),
        Some(1),
        "the object itself still belongs to process 1"
    );
}

#[test]
fn a_transaction_on_the_node_is_forwarded_to_the_owner() {
    let (mut peer, _, node) = peer_with_a_service();
    let req = Frame::plain(F_REQ, 42, PING_TRANSACTION, 0, node, Vec::new());
    let out = peer.handle_tagged(TAG_PRIMARY, req);
    let (dest, forwarded) = out
        .iter()
        .find(|(_, f)| f.ty == F_TXN)
        .expect("the transaction was forwarded");
    assert_eq!(*dest, 1, "to the process that owns the binder");
    assert_eq!(forwarded.target, 7, "addressed by the owner's own wire handle");
    assert_eq!(forwarded.code, PING_TRANSACTION);
    assert_eq!(peer.foreign_forwards, 1);
}

#[test]
fn the_owners_reply_is_relayed_back_as_the_callers_reply() {
    let (mut peer, _, node) = peer_with_a_service();
    let req = Frame::plain(F_REQ, 42, PING_TRANSACTION, 0, node, Vec::new());
    let forwarded = peer
        .handle_tagged(TAG_PRIMARY, req)
        .into_iter()
        .find(|(_, f)| f.ty == F_TXN)
        .expect("forwarded")
        .1;
    // The owner answers the *forwarded* sequence with its own parcel.
    let mut reply_parcel = Parcel::new();
    reply_parcel.write_int32(0x1234);
    let owner_reply = acos_binder_peer::frame_from_parcel(
        acos_binder_peer::F_REP,
        &Frame::plain(acos_binder_peer::F_REP, forwarded.seq, 0, 0, 0, Vec::new()),
        &reply_parcel,
    );
    let out = peer.handle_tagged(1, owner_reply);
    let (dest, relayed) = out
        .iter()
        .find(|(_, f)| f.ty == F_RSP)
        .expect("the reply was relayed");
    assert_eq!(*dest, TAG_PRIMARY, "back to the caller");
    assert_eq!(relayed.seq, 42, "with the caller's own sequence number");
    let mut data = import_parcel_for(TAG_PRIMARY, relayed);
    data.set_data_position(0);
    assert_eq!(data.read_int32().unwrap(), 0x1234, "and the owner's parcel");
}

#[test]
fn a_one_way_transaction_is_forwarded_without_a_pending_reply() {
    let (mut peer, _, node) = peer_with_a_service();
    let req = Frame::plain(
        F_REQ,
        43, PING_TRANSACTION, acos_binder_peer::TF_ONE_WAY, node, Vec::new());
    let out = peer.handle_tagged(TAG_PRIMARY, req);
    assert!(out.iter().any(|(dest, f)| *dest == 1 && f.ty == F_TXN));
    assert_eq!(peer.foreign_forwards, 1);
    // Nothing is waiting for a reply, so a stray F_REP must not be relayed.
    let stray = Frame::plain(acos_binder_peer::F_REP, 1, 0, 0, 0, Vec::new());
    assert!(peer.handle_tagged(1, stray).iter().all(|(_, f)| f.ty != F_RSP));
}

#[test]
fn a_transaction_on_a_dead_owners_node_is_a_structured_refusal() {
    let (mut peer, _, node) = peer_with_a_service();
    peer.disconnect_process(1);
    let req = Frame::plain(F_REQ, 44, PING_TRANSACTION, 0, node, Vec::new());
    let out = peer.handle_tagged(TAG_PRIMARY, req);
    let (dest, reply) = out
        .iter()
        .find(|(_, f)| f.ty == F_RSP)
        .expect("a refusal, never a hang");
    assert_eq!(*dest, TAG_PRIMARY);
    assert_eq!(reply.seq, 44);
    let mut parcel = import_parcel_for(TAG_PRIMARY, reply);
    parcel.set_data_position(0);
    assert_ne!(parcel.read_int32().unwrap(), 0, "a binder exception, not a success");
    assert_eq!(peer.foreign_dead, 1);
}

#[test]
fn the_owner_dying_notifies_every_holder() {
    let (mut peer, _, node) = peer_with_a_service();
    peer.register_process(2, "dev.acos.fixtures.proc-c").unwrap();
    // Process 2 asks for the same object: one node, two holders.
    assert_eq!(peer.foreign_node(1, 7, 2), Some(node));
    assert_eq!(peer.foreign_nodes()[0].3, vec![TAG_PRIMARY, 2]);

    peer.disconnect_process(1);
    let deaths: Vec<(u32, u64)> = peer
        .take_death_frames()
        .into_iter()
        .filter(|(_, f)| f.ty == acos_binder_peer::F_DEATH)
        .map(|(dest, f)| (dest, f.target))
        .collect();
    assert_eq!(deaths, vec![(TAG_PRIMARY, node), (2, node)]);
    // The dead proxy stays addressable (a late call must fail, not resolve to a
    // reused handle); it is freed when its last holder releases it.
    assert_eq!(peer.foreign_owner(node), Some((1, 7)));
    assert_eq!(peer.foreign_node_count(), 1);
}

#[test]
fn a_request_waiting_on_a_dying_owner_is_refused_not_dropped() {
    let (mut peer, _, node) = peer_with_a_service();
    let req = Frame::plain(F_REQ, 45, PING_TRANSACTION, 0, node, Vec::new());
    let forwarded = peer
        .handle_tagged(TAG_PRIMARY, req)
        .into_iter()
        .find(|(_, f)| f.ty == F_TXN)
        .expect("forwarded")
        .1;
    assert!(forwarded.seq > 0);
    peer.disconnect_process(1);
    let out = peer.take_death_frames();
    let (dest, reply) = out
        .iter()
        .find(|(_, f)| f.ty == F_RSP)
        .expect("the waiting caller is answered");
    assert_eq!(*dest, TAG_PRIMARY);
    assert_eq!(reply.seq, 45);
    assert_eq!(peer.foreign_dead, 1);
}

#[test]
fn releasing_the_last_holder_frees_the_node_once_its_owner_is_gone() {
    let (mut peer, _, node) = peer_with_a_service();
    // Another process holds the same object: the caller's release keeps the node.
    peer.register_process(2, "dev.acos.fixtures.proc-c").unwrap();
    assert_eq!(peer.foreign_node(1, 7, 2), Some(node));
    assert!(!peer.release_foreign_holder(node, TAG_PRIMARY));
    assert_eq!(peer.foreign_node_count(), 1);
    peer.disconnect_process(1);
    assert!(peer.release_foreign_holder(node, 2), "the last holder frees it");
    assert_eq!(peer.foreign_node_count(), 0);
    assert_eq!(peer.foreign_released, 1);
}

#[test]
fn the_same_object_gets_one_node_whatever_the_number_of_holders() {
    let (mut peer, _, node) = peer_with_a_service();
    peer.register_process(2, "dev.acos.fixtures.proc-c").unwrap();
    let again = peer.foreign_node(1, 7, TAG_PRIMARY);
    assert_eq!(again, Some(node), "the caller keeps the same node");
    assert_eq!(peer.foreign_node(1, 7, 2), Some(node), "and so does a second holder");
    assert_eq!(
        peer.foreign_node(1, 8, TAG_PRIMARY),
        Some(FOREIGN_FLAG | 2),
        "a different binder is a different node"
    );
    assert_eq!(peer.foreign_node_count(), 2);
}

#[test]
fn a_local_binder_stays_a_wire_handle() {
    // The pre-MPROC3 path must be unchanged: a process's own binder reaches it as
    // its own handle, not as a node.
    let mut peer = Peer::new();
    peer.register_process(1, "dev.acos.fixtures.proc-b").unwrap();
    peer.host_mut().set_binder_owner(java_binder_id(1, 7), 1);
    let mut parcel = Parcel::new();
    parcel.write_strong_binder(java_binder_id(1, 7));
    let exported = peer.export_for_process(1, &parcel);
    let id = exported.binder_ids().into_iter().next().expect("binder");
    assert_eq!(id.0 & JAVA_TAG, JAVA_TAG, "still a Java handle, got {:#x}", id.0);
    assert_eq!(owner_process(id), Some(1));
    assert_eq!(peer.foreign_node_count(), 0);
}
