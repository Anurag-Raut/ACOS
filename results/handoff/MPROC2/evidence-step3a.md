# MPROC2 step 3a evidence — tagged, multi-connection binder peer

Branch `mproc2`, base `mproc1` @ `22e20911`. Commit: this one.

## What changed

1. **Connection handshake** (`crates/acos-binder-peer/src/protocol.rs`,
   `native/binder_driver.c`): new frame type `F_HELLO = 5`. The driver sends it
   once, right after `connect()`, before any transaction:
   `code` = process tag, `data` = process name (≤ 64 bytes). The tag comes from
   `ACOS_PROCESS_TAG` (default 0 = the session's primary process) and the name
   from `ACOS_PROCESS_NAME`. `launch_session` now sets both for the primary
   guest (`ACOS_PROCESS_TAG=0`, `ACOS_PROCESS_NAME=<package>`).
   A peer that sees no hello treats the connection as tag 0, so an older driver
   is never mis-addressed; that is also the one-guest fast path, byte for byte.

2. **N tagged connections** (`serve.rs`, new; `crates/acos-art/src/launch.rs`):
   the launcher's per-connection loop is replaced by
   `acos_binder_peer::serve::serve(peer, listener, stop)` — one poll loop over
   the listener plus every accepted connection, single-threaded (one
   `ServiceHost`, no lock, reverse deliveries keep their order). The listener
   now stays open for the whole session (a later process can connect); it is
   closed when `serve` returns. Handshakes are validated: out of range (> 63),
   duplicate tag, or a full process table is a `TagRefusal` with a stable code
   (`PROCESS_TAG_OUT_OF_RANGE` / `PROCESS_TAG_IN_USE` / `PROCESS_TABLE_FULL`),
   a status-bearing reply, and the connection is closed — never a silent fold
   into another process.

3. **Process-tagged binder ids** (`peer.rs`): a driver wire handle is only
   unique inside its own guest, so `JAVA_TAG | wire` would make two processes
   collide on one `BinderId`. Bits 34..40 of a `JAVA_TAG` id now carry the
   owning process (`java_binder_id(tag, wire)`, `owner_process(id)`); bits 32/33
   (`JAVA_TAG`/`NODE_TAG`) and 40 (`LAUNCH_NODE`) keep their meaning.

4. **Per-process state in the peer**: `ProcessConn` holds the Java binder table
   and the whole launch state (`bindApplication` bytes, `ClientTransaction`,
   framework-marshalled reply templates, `IApplicationThread` wire handle,
   delivered flags) per tag — a second process performs its own
   `attachApplication`/`bindApplication` and must not overwrite the first's.
   `Peer::handle` keeps its old signature (tag 0); `Peer::handle_tagged` returns
   `Vec<(tag, Frame)>` so the caller writes each frame to the connection that
   owns it. The Java binder cap is now **per process** (one process filling its
   table cannot deny another), still with a structured `ART_RESOURCE_LIMIT`.

5. **Binder ownership in the service layer** (`acos-services/src/host.rs`):
   `ServiceHost` records the owning process of every app binder it is handed
   (`set_binder_owner` / `binder_owner` / `binders_of_process` /
   `drop_process_binders`), bounded by `MAX_BINDER_OWNERS = 8192` with a counted
   refusal (`binder_owner_drops`). The peer records owners from `addService`,
   `attachApplication`, and every binder object in a request parcel
   (`Parcel::binder_ids()`, new: the peer installs the frame's object offsets
   with `Parcel::unmarshall_with_objects`, which it previously dropped).
   `disconnect_process(tag)` drops exactly that process's binders in the peer and
   in the host; a later delivery to one of them is a structured
   `UNSUPPORTED_ANDROID_API` record, not a silent drop.

6. **Cross-process binder transfer is refused, not mis-routed**: a reply that
   would hand process A a binder owned by process B is answered with a
   structured `UNSUPPORTED_ANDROID_API` (`<binder-transfer>`) instead of writing
   B's wire handle into A's reply (which would make A call a *different object*
   and report success). Step 3c needs the peer-side proxy/node table described
   in `STATUS.md`.

No guest-visible change: no new AIDL surface, no new permission, the same
service graph, the same `ServiceManager` names.

## Unit tests (new file `crates/acos-binder-peer/tests/mproc2_process_tags.rs`)

`cargo test -p acos-binder-peer` → 5 + 11 + 15 passed, 0 failed
(`results/handoff/MPROC2/evidence/unit-tests-peer.txt`). The 11 new tests:

- `a_handle_is_scoped_to_its_process` — wire handle 7 registered by both
  processes under different names; distinct `BinderId`s; `owner_process` round
  trips; a host node has no owner.
- `the_service_host_records_the_owner_of_every_app_binder`
- `a_parcel_imported_from_two_processes_never_collides` — the same wire frame
  imported as process 1 and as process 2 yields different ids with the same low
  32 bits; `import_parcel` stays tag 0.
- `a_delivery_reaches_the_process_that_owns_the_target_binder` — an
  `IResultReceiver.send` delivery queued for process 2's binder is framed for
  process 2's connection (and the mirror case for process 1).
- `a_connection_without_a_handshake_is_the_primary_process`
- `a_duplicate_tag_is_refused_and_the_first_connection_keeps_it`
- `an_out_of_range_or_full_process_table_is_refused` (+ the three stable codes)
- `a_disconnected_process_drops_only_its_own_binders`
- `a_delivery_to_a_dead_process_is_a_structured_record_not_a_silent_drop`
- `the_java_binder_cap_is_per_process`
- `the_serve_loop_accepts_two_tagged_connections_and_answers_each` — a real
  `SOCK_SEQPACKET` listener, two clients with hello + `PING_TRANSACTION`,
  each gets *its own* reply seq, `accepted=2`, `peak_connections=2`,
  `closed=2`. (This test is what caught an index-out-of-bounds in the first
  version of the poll loop, which the ART run below then also hit before the
  fix.)

`cargo test -p acos-parcel -p acos-services` → 5 + 90 passed, 0 failed (with the
worktree test env: `fixtures/build` hardlinked, `ACOS_FONT_DIR` set; the two
env-dependent tests fail without that setup and pass with it, unchanged by this
branch).

## Real ART run (single-app regression, no guest-visible change)

`tools/art-runtime/run-exclusive.sh --task MPROC2 -- tools/guarded.sh --mem 8G --
env -u ACOS_HOME .cache/mproc2-target/debug/acos verify scenarios/counter.yaml
--engine art` → 22 steps, 21 pass; the only non-pass is the **pre-existing**
`back: strict settle timed out (no-draw-after-action)` already recorded in
`results/handoff/ROT1.md` and `results/handoff/MPROC1/evidence-step1.md`. The
session itself launched, ran and tore down normally (peer evidence below).

Direct launch of the counter fixture, with the launcher's stderr kept in
`evidence/launch-counter.stderr.txt`:

```
acos-binder-peer: process 0: android.hardware.display.IDisplayManager.getDisplayInfo answered
  from a framework-marshalled reply (232 bytes)        (repeated: the guest's own template)
acos-binder-peer: process tag 0 disconnected
                  (0 java binders dropped)
acos-binder-peer: process 0 connection closed (0 java binders, 17 host binder records dropped)
acos-art: peer: processes=[] peak_connections=1 accepted=1 refused=0 closed=1 frames=134/119 unroutable=0
acos-art: launch wall=169ms until=activity-resumed application_created=false activity_resumed=true
          txn=None/false reverse_replies=3 reverse_failed=0 logd=84 peer_resource_limits=0
```

Reading it: one connection, one accepted handshake (tag 0), 134/119 frames
through the poll loop, 17 app binders recorded with owner tag 0 and all dropped
when the guest's connection closed, no unroutable delivery, no resource limit.
`code: GUEST_EXITED` in the launch JSON is the counter fixture exiting by
itself after `--until activity-resumed`, not a peer failure.

## Cost when no second process exists

One 48-byte frame per session (the hello), one `u64` shift per imported/exported
Java binder id, one `Option<ProcessConn>` slot per tag, and one `poll()` over
(listener + 1) instead of a blocking `read_frame` per frame. No new thread, no
new socket, no new service, no extra guest work. The prepared A/B is
`tools/mproc2-ab.sh` (arms `mproc1`/`mproc2`, phases `memory,cold,latency`, no
extra APKs) — **prepared, not run** (the common brief: the bench owner runs it).
