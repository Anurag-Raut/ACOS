# MPROC2 status (branch `mproc2`, base `mproc1` @ `22e20911`)

**Step 3a is done, tested and committed. Steps 3b, 3c and 4 are not started.**

The run's budget went into 3a, which is the load-bearing seam the other three
steps address (process tag on the connection, `tag -> BinderId` tables, per-
process binder records, N accepted connections), plus the real-ART proof that a
single-app session is unchanged.

| step | state |
|---|---|
| 3a peer tagging | **done** — handshake, `ProcessConn` per tag, process-tagged `BinderId`s, `ServiceHost` binder owners, N-connection poll loop, 11 unit tests, real ART smoke |
| 3b lazy spawn | not started (design below) |
| 3c cross-process | not started; one wall identified (below) |
| 4 idle kill | not started (design below) |
| A/B | prepared, not run: `tools/mproc2-ab.sh` (arms `mproc1`/`mproc2`) |

Evidence: `results/handoff/MPROC2/evidence-step3a.md`,
`results/handoff/MPROC2/evidence/launch-counter.stderr.txt`,
`results/handoff/MPROC2/evidence/unit-tests-peer.txt`.

## Step 3a — what exists now (the contract 3b/3c/4 build on)

- `F_HELLO` handshake (`protocol.rs`, driver `connect_peer`): `code` = tag
  (0..=63), `data` = name (≤ 64 B), sent once after `connect`, before any
  transaction. `ACOS_PROCESS_TAG` / `ACOS_PROCESS_NAME` in the guest env; the
  launcher sets tag 0 + the package name for the primary guest. No hello = tag 0.
- `acos_binder_peer::serve::serve(peer, listener, stop)` — one poll loop,
  listener **stays open** for the session, accepts up to `MAX_PROCESSES = 64`
  tagged connections, validates tags (`TagRefusal` with stable codes), routes
  every returned `(tag, Frame)` to that tag's socket, and on close calls
  `Peer::disconnect_process(tag)` (drops that process's Java binders in the peer
  and its binder owner records in the host). `ServeOutcome` reports
  `peak_connections / accepted / refused / closed / frames_in / frames_out /
  unroutable`; the launcher logs it as `acos-art: peer: processes=… accepted=…`.
- `Peer`: per-process state (`ProcessConn` + `LaunchState`), `register_process`,
  `disconnect_process`, `tags()`, `process_name`, `java_binders_for(tag)`,
  `all_java_binders()`, `launch_*_for(tag)`, `handle_tagged(tag, frame)`; the
  old `handle`/`launch_*`/`java_binders()` still mean tag 0.
- `java_binder_id(tag, wire)` / `owner_process(id)`: the owning process lives in
  bits 34..40 of a `JAVA_TAG` id; `java_wire` still reads the low 32.
- `ServiceHost`: `set_binder_owner`, `binder_owner`, `binders_of_process`,
  `drop_process_binders`, `binder_owner_drops`, `queue_delivery` (a delivery
  queued outside a handler call — the lifecycle/ctl path needs it),
  `MAX_BINDER_OWNERS = 8192`.
- `Parcel::binder_ids()` + `Parcel::unmarshall_with_objects()`: an imported
  parcel now keeps its object table, so "which binders did this process hand
  over" is answerable.
- A reply that would hand process A a binder owned by process B is refused with
  a structured `UNSUPPORTED_ANDROID_API` (`<binder-transfer>`) instead of
  writing B's handle into A's reply.

## Step 3b — lazy spawn (next; the design is already concrete)

1. `zygote_prod` keeper FIFO `spawn <tag>` (already exists, ZPROD P2) gains a
   per-tag variant carrying `{tag, package, uid, apk_guest_path,
   native_library_dir, guest_process_name}`; the child gets
   `ACOS_PROCESS_TAG=<tag>`, its own run root and its APK bound at
   `/data/app/<name>` (already staged by `extra_apks`), and connects to the same
   `peer.sock` the primary uses (the `sockets` dir is already bound at
   `/dev/socket`; the fallback when the zygote is disabled is a direct
   `acos-art launch --tag <tag>`).
2. Trigger: in the PMS/ATMS resolution path, when `queryIntentActivities` /
   `resolveIntent` / `resolveService` / a provider lookup resolves to a
   component of an installed extra package and that tag has no live connection,
   spawn it (once; the spawn is the tag's `ProcessRecord` creation too).
3. ATMS/AMS: one `ProcessRecord` per tag (`uid`, `package`, `pid`, `state`,
   `spawned_by`, activities list). `foreground = the process of the resumed
   activity`, `A` goes `paused`/`stopped` when B's activity is on top.
4. `bindApplication` for the second package: the peer already keeps its own
   `bindApplication` bytes + `IApplicationThread` per tag (3a), so the existing
   launch delivery code works per tag once the launcher queues the second
   guest's frame.

The remaining risk for 3b is mount/pid namespaces: every guest currently gets
its own run root, so the second guest must mount **the same**
`<run>/socket` (read-only) and the **same** `app`/`app-lib` trees. That is a
`launch_session` refactor (extract "session mount plan" from "spawn one guest"),
which is where the next session should start.

## Step 3c — cross-process transactions (the one wall)

Starting B's activity and finishing it with `setResult` needs no new binder
work: it is ATMS state plus the existing `ActivityResultItem` delivery, and the
result goes back to A's `IApplicationThread`, which 3a already addresses per
process.

**AIDL round trip to an object that lives in B does.** A `BINDER_TYPE_HANDLE`
only means something inside the process that owns it, and the driver has no way
to name "a binder of another process": if A is handed a proxy to B's service,
the wire form carries a handle number in A's space (the peer currently refuses
that reply, see above). The fix stays inside the decided design ("the Rust
service layer routes transactions between processes") and needs no driver
change:

- the peer allocates a **synthetic peer node** `FOREIGN_BASE + n` for a
  `(owner_tag, wire)` pair the first time it must hand that binder to another
  process, and remembers `n -> (owner_tag, wire)`;
- the exporter writes `BINDER_TYPE_BINDER(n)` (a node, not a handle), so A's
  driver maps `n` to a normal remote handle and A's transactions to it arrive as
  `F_REQ { target: n }`;
- the peer forwards such a transaction to the owner's connection as an `F_TXN`
  with the owner's wire handle, keeps `(A, seq) <-> (B, seq)`, and relays B's
  `F_REP` back to A as an `F_RSP`. One-way is a fire-and-forget; a dead owner is
  a structured `DEAD_OBJECT` reply.

That is a contained change in `peer.rs`/`serve.rs` (a mapping table plus one
relay), but it is *new transaction-routing design* inside the peer, so it is
flagged for the orchestrator rather than decided silently. Fixtures: extend
`fixtures/apps/multi-package-probe` with a `proc-a`/`proc-b` pair (A binds B's
service and does the AIDL round trip; A starts B's activity and receives the
`setResult`).

## Step 4 — idle kill (design)

- Per-tag idle clock in the launcher, driven by ATMS foreground transitions
  (`--idle-kill-seconds`, default 30). Only background tags are candidates.
- On expiry: `queue_delivery` an `IApplicationThread.scheduleSaveInstanceState`
  to the tag's `IApplicationThread` (the binder owner table already names it),
  wait for the reply, then `kill <tag>` through the zygote FIFO (never `pkill`),
  `Peer::disconnect_process(tag)`, and report
  `killed: {tag, reason:"idle", saved:true}` in the reply and `verdict.json`.
- Recreate on demand: the same lazy-spawn path as 3b, and the saved
  `Bundle` is replayed in the `bindApplication`/activity-launch arguments; the
  reply reports `recreated`.
- Proof: an ART scenario that starts B, backgrounds it, advances the clock
  (configurable seconds → the scenario sets `--idle-kill-seconds 2`), asserts
  the kill record, then calls back into B and asserts it is spawned again with
  its state. Must run guarded + run-exclusive.

## Notes for the orchestrator

1. **The 3a seam is where the risk was**, and it is now covered by unit tests
   (including the real serve loop) and by a real ART session: one connection,
   `accepted=1`, 17 owner records, `unroutable=0`, no resource limit.
2. **3c needs one decision**: the peer-side foreign-node table above (new
   routing in the peer) — confirm it, or say that 3c's AIDL round trip is
   limited to interfaces the host serves.
3. `tools/mproc2-ab.sh` is prepared and **not run** (arms `mproc1`/`mproc2`).
   Expectation: the hello frame + one `poll()` is inside the noise floor.
4. The counter scenario's only non-pass is the pre-existing `back`
   no-draw-after-action timeout (ROT1/MPROC1 evidence), not this branch.
