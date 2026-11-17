# MPROC3 status (branch `mproc3`, base `mproc2` @ `09dadf85`)

**Step 3c's peer layer is done, tested and committed. Steps 3b and 4 are not
started, and 3c has no real two-process ART proof yet (it needs 3b's spawn).**

The run's budget went into the routing seam the orchestrator confirmed, because
it is the piece that 3b and 4 both sit on: a binder that lives in another app
process can now be handed across processes, called, answered, and observed to
die. Everything below is unit-proven in `crates/acos-binder-peer`; nothing was
half-written into the spawn path.

| step | state |
|---|---|
| 3b lazy spawn | **not started** (design in `results/handoff/MPROC2/STATUS.md`, seam re-verified below) |
| 3c foreign nodes | **peer layer done** — nodes, forwarding, reply relay, death fan-out, holder release, 10 new tests. Driver half (`F_DEATH` → `BR_DEAD_BINDER`, `BC_RELEASE` → release frame) and the A/B fixtures are **not** done |
| 4 idle kill | not started |
| A/B | still only prepared (`tools/mproc2-ab.sh`), not run |

Evidence: `evidence-step3c-peer.md`, `evidence/unit-tests-peer-mproc3.txt`.

## What exists now (commit "MPROC3 step 3c: foreign-node routing")

- `Peer::export_for_process(dest, parcel)` is now the **single routing point**
  for every parcel the peer writes into a process's connection (host reply,
  reverse delivery, forwarded transaction, relayed reply). A binder owned by
  `dest` itself stays `BINDER_TYPE_HANDLE(wire)` — the pre-MPROC3 bytes, byte for
  byte. A binder owned by *another* process becomes
  `BINDER_TYPE_BINDER(FOREIGN_FLAG | n)`.
- `FOREIGN_FLAG = 1 << 41` names a synthetic node. `Peer::foreign_node(owner,
  wire, holder)` allocates one node per `(owner tag, owner wire)` pair —
  one object, N proxies — bounded by `MAX_FOREIGN_NODES = 4096` with a counted
  `ART_RESOURCE_LIMIT`, never a fabricated handle. `foreign_owner(node)` maps
  back; `foreign_nodes()` is the evidence a reply/verdict can carry.
- A transaction on a foreign node (`F_REQ target = node`) is **forwarded to the
  owner's connection** as `F_TXN` with the owner's own wire handle
  (`Peer::forward_foreign`), the peer remembers
  `(owner, forwarded seq) -> (caller, caller seq, node)`, and the owner's `F_REP`
  is **relayed back** to the caller as its own `F_RSP` (`relay_foreign_reply`),
  re-exported so a reply that carries binders routes the same way. One-way
  transactions are fire-and-forget (no pending entry). A `TF_STATUS_CODE` reply
  (libbinder never reached `onTransact`) is a structured refusal, not a success.
- **Death**: `disconnect_process(tag)` calls `release_foreign_of(tag)`, which
  drops the `(owner, wire)` index entries of the dead process, queues one
  `F_DEATH` per holder, and answers every forwarded request still waiting with a
  structured refusal. The dead proxy stays addressable while a holder has it (a
  late call must fail, not resolve to a recycled handle) and is freed when its
  last holder releases or disconnects (`release_foreign_holder`,
  `release_foreign_holders_of`). `serve` writes the queued frames either through
  the normal per-frame path (`handle_tagged` returns them tagged) or through
  `flush_deaths` right after `close_conn`.
- `ServeOutcome::deaths` counts the death notifications a session actually
  wrote; `Peer::foreign_forwards / foreign_dead / foreign_released /
  foreign_node_count()` are the routing evidence.

### Tests (new file `crates/acos-binder-peer/tests/mproc3_foreign.rs`, 10)

`cargo test -p acos-binder-peer` → 5 + 11 + 10 + 15 passed, 0 failed
(`evidence/unit-tests-peer-mproc3.txt`). The new ones:

- `a_binder_of_another_process_reaches_the_caller_as_a_node` — the caller imports
  a node, the object still belongs to the other process.
- `a_transaction_on_the_node_is_forwarded_to_the_owner` — `F_TXN` to the owner's
  connection, addressed by the owner's wire handle.
- `the_owners_reply_is_relayed_back_as_the_callers_reply` — the caller's own seq
  and the owner's parcel.
- `a_one_way_transaction_is_forwarded_without_a_pending_reply`
- `a_transaction_on_a_dead_owners_node_is_a_structured_refusal`
- `the_owner_dying_notifies_every_holder` — two holders, two `F_DEATH`, one node.
- `a_request_waiting_on_a_dying_owner_is_refused_not_dropped`
- `releasing_the_last_holder_frees_the_node_once_its_owner_is_gone`
- `the_same_object_gets_one_node_whatever_the_number_of_holders`
- `a_local_binder_stays_a_wire_handle` — the one-process path is unchanged.

## What 3c still needs (in order)

1. **Driver half** (`crates/acos-binder-peer/native/binder_driver.c`, keep it in
   functions the `bindercb` branch does not touch — it rewrites
   `deliver_incoming`/`node_by_wire`):
   - `dispatch_frame`: handle `F_DEATH` (target = node) by pending
     `BR_DEAD_BINDER` + the cookie of every `BC_REQUEST_DEATH_NOTIFICATION` the
     guest registered for that node's local handle;
   - `handle_bc`'s `BC_REQUEST_DEATH_NOTIFICATION` case currently only skips the
     12 bytes ("peer liveness not modelled") — record `(handle, cookie)` there;
   - `BC_RELEASE`/`BC_CLEAR_DEATH_NOTIFICATION` → send one small frame so the
     peer can drop its holder reference (that is the "refcounts released when A
     drops it" half of the decision);
   - an `F_RSP` variant that produces `BR_DEAD_REPLY` (so a dead object throws
     `DeadObjectException`, not `RuntimeException`).
   None of this is written yet: it is unverifiable until two real processes exist,
   and shipping unexercised C in the driver is what the rules forbid.
2. **3b, the spawn** — this is the gate for any real 3c/4 proof. The seam is
   intact and small: the sandbox already binds every extra APK
   (`sb.ro_bind(&extra.staged, &extra.guest_path)` /
   `extra.guest_lib_root` in `launch.rs`), the peer already keeps N tagged
   connections and per-tag launch state, and `ServiceHost` already records binder
   owners. What is missing is (a) a per-process run root + `ACOS_PROCESS_TAG`
   for the second guest (the primary's `sockets.0` dir is already the one both
   would bind at `/dev/socket`, so they share `peer.sock`), (b) a spawn request
   path from the peer thread (which owns the `ServiceHost`) to the launcher, and
   (c) the second guest's launch `ClientTransaction` (host-side marshalling, or
   the requested component as that guest's launcher activity for a first proof).
3. **Fixtures** — `fixtures/apps/proc-a` / `proc-b` (A starts B's activity,
   `setResult`/`finish`, A binds B's service and does an AIDL round trip, B
   killed → A's `binderDied`), scenario `mproc3-two-process`.
4. **Step 4** — idle kill, as designed in `MPROC2/STATUS.md` §4.

## Notes for the orchestrator

1. The routing decision you confirmed is implemented exactly as scoped
   (synthetic node per `(owner tag, wire)`, `F_REQ -> F_TXN` forward, `F_RSP`
   relay, death on disconnect, bounded tables) and it lives in the Rust service
   layer, not in the driver.
2. No guest-visible change and no AIDL change: one process's session is byte for
   byte what it was (`a_local_binder_stays_a_wire_handle` pins it).
3. Disk: the host was at 84 % when this run started and 86 % after the build;
   the target dir (417 MiB) was deleted at the end. Do not start 3b on a host
   above 85 % — a full ART run plus a fixture build needs room.
