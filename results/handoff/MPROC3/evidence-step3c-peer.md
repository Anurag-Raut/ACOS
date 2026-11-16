# MPROC3 step 3c evidence — cross-process binder routing (peer layer)

Branch `mproc3`, base `mproc2` @ `09dadf85`. Commit: this one.

## What changed

1. **`protocol.rs`**: `F_DEATH = 6` (peer → driver: a synthetic node handed to
   this process died). `FOREIGN_FLAG = 1 << 41` and `MAX_FOREIGN_NODES = 4096`
   live in `peer.rs` next to the other id tags (bit 32 Java handle, bit 33 host
   node, bits 34..40 process tag, bit 40 `acos.launch`).

2. **`peer.rs`, one export point.** `Peer::export_for_process(dest, parcel)`
   rewrites a reply/delivery parcel object by object:
   - a binder owned by `dest` keeps `BINDER_TYPE_HANDLE(wire)` — the pre-MPROC3
     encoding, unchanged;
   - a binder owned by another process becomes
     `BINDER_TYPE_BINDER(FOREIGN_FLAG | n)`, because a wire handle means nothing
     outside its own driver. `frame_from_parcel` now keeps such an id verbatim
     (its binder branch had no case for it and would have OR-ed `NODE_TAG` in).
   It is called from every place the peer writes into a connection: the host
   service reply, reverse deliveries, the forwarded transaction and the relayed
   reply.

3. **Node table.** `foreign: HashMap<node, ForeignNode{owner_tag, wire, holders}>`
   plus `foreign_index: HashMap<(owner_tag, wire), node>` — one node per object,
   one holder list per node, `MAX_FOREIGN_NODES` bounded with a counted
   `ART_RESOURCE_LIMIT` refusal (never a fabricated handle). A node handed out
   for an owner that is already dead queues its `F_DEATH` at once (a proxy to a
   dead object), which is what a kernel driver reports on the first reference.

4. **Forwarding and relay.** `F_REQ target = node` → `forward_foreign`:
   `F_TXN` to the *owner's* connection with the owner's wire handle as target,
   `pending_foreign[(owner, forwarded seq)] = (caller, caller seq, node)`;
   the owner's `F_REP` → `relay_foreign_reply` → `F_RSP` to the caller with the
   caller's seq and the owner's parcel (re-exported, so a reply that returns
   another process's binder routes recursively). `TF_ONE_WAY` forwards without a
   pending entry. `TF_STATUS_CODE` (libbinder never reached `onTransact`) is a
   structured refusal. The serve loop needed **no** change: `handle_tagged`
   already returns `(tag, Frame)` pairs and the loop writes each to that tag's
   connection.

5. **Death.** `disconnect_process(tag)` → `release_foreign_of(tag)`:
   - the tag's `(owner, wire)` index entries go (so a restarted owner's reused
     wire number becomes a new object with a new node);
   - every holder gets one `F_DEATH` (nodes stay addressable: a late call must
     fail, not resolve to a recycled handle);
   - every forwarded request still waiting is answered with a structured refusal
     and counted in `foreign_dead`;
   - nodes nobody holds are freed;
   - `release_foreign_holders_of(tag)` drops the references the dead process
     held *as a holder* of other processes' nodes.
   `release_foreign_holder(node, holder)` frees the node when the last holder
   goes. This is the peer half of the driver's `BC_RELEASE`/death-notification
   contract (see `STATUS.md` §"What 3c still needs").

6. **`serve.rs`**: `ServeOutcome::deaths` counts death notifications written;
   `flush_deaths` writes the frames queued by `close_conn` (the disconnect path
   does not run through `handle_tagged`), and the normal per-frame path writes
   them when `handle_tagged` returns them.

No guest-visible change: no new AIDL surface, no new service, no new socket, no
new thread. The one-process session is byte for byte what it was — pinned by
`a_local_binder_stays_a_wire_handle`.

## Unit tests

`cargo test -p acos-binder-peer` → **5 + 11 + 10 + 15 passed, 0 failed**
(`evidence/unit-tests-peer-mproc3.txt`). The 10 new tests are in
`tests/mproc3_foreign.rs` (listed in `STATUS.md`). Notable ones:

- `a_binder_of_another_process_reaches_the_caller_as_a_node` — the caller imports
  a node (`FOREIGN_FLAG`), while `ServiceHost::binder_owner` still says the
  object belongs to process 1.
- `the_owners_reply_is_relayed_back_as_the_callers_reply` — forwarded seq differs
  from the caller's seq; the caller gets its own seq and the owner's `0x1234`.
- `the_owner_dying_notifies_every_holder` — two holders, two `F_DEATH` frames,
  one node; the proxy stays addressable.
- `a_request_waiting_on_a_dying_owner_is_refused_not_dropped` — the in-flight
  `F_REQ 45` gets a structured refusal.
- `releasing_the_last_holder_frees_the_node_once_its_owner_is_gone`.

## What this does NOT prove (honest)

- No real two-process ART run: there is no second process until step 3b, so the
  `F_DEATH` → `BR_DEAD_BINDER` path (driver) and the AIDL round trip between two
  guest processes are **not** exercised yet. The driver half is not written; it
  would be unverifiable C in the code path every session uses.
- `serve`'s socket-level delivery of `F_DEATH` is covered only by the peer-level
  tests above (the frame is queued and returned tagged); the existing serve-loop
  test (`mproc2_process_tags.rs`) still covers N tagged connections and was not
  extended to deaths in this run.
- `BC_RELEASE` → peer release is not wired: `release_foreign_holder` exists and
  is tested, the driver does not send the frame yet.

## Cost

Per exported object: one `HashMap` lookup; per foreign transaction: one extra
frame each way (A→peer→B, B→peer→A) and one table entry until the reply; per
disconnect: one walk of the node table (≤ 4096 entries) and one frame per holder.
No cost when no process ever holds another process's binder: `export_for_process`
returns a clone of the parcel after an empty `binder_ids()` scan (the scan is the
same one MPROC2's refusal path already did on every reply).
