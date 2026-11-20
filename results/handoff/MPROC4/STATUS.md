# MPROC4 status (branch `mproc4`, base `mproc3` @ `edc84490`)

**Step 3b is done and proven on real ART: a second guest process exists, is
spawned lazily by a cross-package component resolution, and the reply/process
list names both processes. Steps 3c and 4 are not done.**

| step | state |
|---|---|
| 3b lazy spawn | **done** — per-tag run root + `ACOS_PROCESS_TAG`/`ACOS_PROCESS_NAME`, shared `peer.sock`, spawn request from the peer thread to the launcher, `ProcessRecord` per tag, second guest's own `bindApplication` + launch `ClientTransaction` delivered by the host. Real ART: `peak_connections=2 accepted=2`, both processes in the table |
| 3c fixtures + scenario | **partial** — `fixtures/apps/proc-a` + `proc-b` exist and run (A starts B, B logs its own lifecycle, A can bind B's service); the ACOS **flow** of 3c (pause/stop of A, B's window on top, `setResult` → A's `onActivityResult`, the AIDL round trip) does **not** work yet; `scenarios/mproc3-two-process.yaml` intentionally not written (a scenario that cannot pass would be a fake spec); the driver half of the peer's death path (`F_DEATH` → `BR_DEAD_BINDER` etc.) is **not** written |
| 4 idle kill | not started |
| A/B (`tools/mproc2-ab.sh`) | still prepared, not run (correct: the bench owner schedules it) |

Evidence: `evidence-step3b.md`, `evidence/run3/` (driver output, launcher
stderr, session evidence), `evidence/step3b-peer-lines.txt`,
`evidence/step3b-unit-tests-services.txt`.

## What exists now (commit "MPROC4 step 3b: …")

- **Spawn request path.** `AmsState::request_process(package, class, reason)` is
  called where a component resolves to a package (`startActivity`,
  `startService`, `bindService`), deduped per `(component, reason)` and bounded
  (`MAX_SPAWN_REQUESTS = 64`). `AtmsService::take_spawn_requests` drains it;
  `acos_binder_peer::serve::serve_with_spawns(peer, listener, stop, sink)` hands
  each request to the **launcher** through a channel — the peer thread never
  starts a process, and the launcher never guesses a component.
- **Per-tag process records.** `ProcessRecord` carries its `tag`; `AtmsState`
  keeps one per process (`process_started`, `attach_application_for`,
  `finish_attach_application_for`, `process_list`). The tag of an
  `attachApplication`/`setRenderThread`/activity token is decoded from the peer's
  binder id (`process_tag_of`, bits 34..40) — the same number the driver's hello
  claimed. `Peer::register_process` seeds the record at the handshake, exactly
  where `AMS.startProcessLocked` creates it on a device. Tag 0 keeps the old
  synthesised `bindApplication`/launch transaction; a secondary process marshals
  its own (the guest is this session's app side) and the peer delivers it as
  `F_TXN` — host-marshalled transport, guest-marshalled content, no invention.
- **Launcher.** `SecondarySpec` (per installed extra: staged APK, guest paths,
  uid, activity, app class/theme/flags/labels, its own run root and `out`),
  `SecondaryShared` (the session inputs both guests share: boot image, shims,
  native-lib root, **the same** `/dev/socket` and `peer.sock`, overlays, geometry)
  and `SecondaryPool` (spawn/reap/shutdown, bounded records, `ctl processes`
  module). A secondary guest gets its own cgroup scope, process group,
  `PDEATHSIG`, `ACOS_PROCESS_TAG`, `ACOS_PROCESS_NAME`, and a live probe whose
  samples feed the session's tree. `launch --out` now also reports
  `processes: {activity_manager: [...], launcher: {...}}` when a session had more
  than the primary process.
- **Guest.** One branch in the start-activity guard: a component whose package is
  not this process's package is **not** instantiated here
  (`ACOSLAUNCH MARK start-activity-remote`), because it belongs to another
  process. Before this, proc-a tried to load proc-b's class in its own process
  and reported `UNSUPPORTED_ANDROID_API`.
- **Fixtures.** `fixtures/apps/proc-a` (`startActivityForResult` to proc-b,
  `bindService`, `transact`, `unbindService`, lifecycle + result + transaction
  `ACOSPROBE` lines) and `fixtures/apps/proc-b` (activity with
  `setResult`+`finish`, `EchoService` with a raw `Binder.onTransact` — the same
  `transact` entry point every AIDL stub is reached through, without needing
  `aidl` in the fixture build). Both built by `tools/build-apps.sh`.

## What 3c still needs, in order

1. **A's pause/stop and B's window on top.** The host knows B's activity resumed
   (token adopted), but nothing pauses A: on a device `ATMS` queues
   `PauseActivityItem`/`StopActivityItem` for the previous top and
   `ResumeActivityItem` for the new one. The delivery transport already exists
   (`PendingDelivery` → `IApplicationThread`); what is missing is the
   top-of-task bookkeeping *per process* and the two transactions. Until then the
   probe tree keeps showing A's window (measured: `proc-b-window-on-top` FAIL,
   `proc-a-paused-or-stopped-under-b` FAIL in `evidence/run3`).
2. **The activity result across processes.** When B `setResult`+`finish`es, the
   host has A's token (`PendingLaunch.result_to`) and must queue one
   `ActivityResultItem` transaction to **A's** app thread. `PendingLaunch` is
   consumed by `adopt_launch_token`, so result routing needs the host to keep the
   result receiver alongside the activity record.
3. **The binder round trip.** `bindService` to another package's service needs the
   host to answer `connected(connection, binder)` with **B's** binder: B's guest
   returns it from `Service.onBind`, the peer exports it to A as a synthetic
   foreign node (already implemented and unit-tested in MPROC3), and A's
   `transact` is forwarded. What is missing is the guest-side `bindService` path
   for a service that is not in this process (and the host marshalling of
   `IServiceConnection.connected`).
4. **The death path in the driver** (`F_DEATH` → `BR_DEAD_BINDER` + cookies,
   `BC_RELEASE` → release frame, `F_RSP` → `BR_DEAD_REPLY`) — keep it in functions
   the `bindercb` branch does not rewrite (`deliver_incoming`/`node_by_wire`), as
   the orchestrator decided. It is only verifiable once 3 and 4 above can kill B
   and observe `binderDied` in A.
5. **`scenarios/mproc3-two-process.yaml`** once the flow above passes — the
   driver (`tools/mproc4-two-process-drive.py`) already carries the assertions
   (`OPEN_3C` lists exactly the ones that still fail).
6. **Step 4** (idle kill) as designed in `MPROC2/STATUS.md` §4; the spawn path it
   needs is now in place.

## Notes for the orchestrator

1. The spawn trigger is deliberately split: the **resolution path records the
   need**, the **launcher decides**. No pid, tag or command line is invented in
   the service layer, and a request for a package the session never installed is
   reported (`uninstalled`), never silently dropped.
2. `until=activity-resumed` is used for the secondary guest, so the second
   process launches *the requested component* (the MPROC2 design's sanctioned
   first proof). A subsequent `startActivity` into an already-running process
   still has no materialisation path — that is 3c item 1/3, and it fails
   honestly (the guest returns `START_CANCELED`-equivalent, not a fake success).
3. Known gap in the same area: `proc-b` saw `note=none` — the launch intent's
   extras are recorded in the host's `AmsState`, but the second process is
   started from its argv, so intent extras do not cross yet. It is part of 3c
   item 2 (the launch must be the recorded `PendingLaunch`, not just a component
   name).
4. Disk: host at 84 % when this run started; the target dir and the ART data dir
   were deleted at the end.
