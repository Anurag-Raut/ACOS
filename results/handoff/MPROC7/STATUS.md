# MPROC7 status (branch `mproc7`, base `mproc5c` @ `a3ff535b`)

**Every launch flavour in this brief is now a host-built `ClientTransaction`
delivered on the existing transport, the way system_server does it
(ADR-0029 amendment).** The activity tokens a host launch carries are
system_server-side binders (`ServiceHost::register_activity_token`, the id
encodes the process tag like a guest-minted token does), the activity record is
created before the guest reports `activityResumed`, and the guest's own
`start-activity` guard no longer instantiates a second activity for a launch the
host owns.

| step | state |
|---|---|
| 0 `launch_hang::ordinary_launch…stays_fast` red? | **fixed: it was a MPROC2 regression, not the environment** (see §0) |
| 1 launch into an already-running process | **done** — host encoder + host token; intent extras cross; MPROC4's guest-marshalled first launch of a spawned process moved onto the same encoder |
| 2 `startActivityForResult` A→B→A | **done** — `ActivityResultItem` + `ResumeActivityItem` queued to A's app thread; driver row green, scenario asserts it |
| 3 `singleTop` / `FLAG_ACTIVITY_SINGLE_TOP` | **done** — `NewIntentItem(resume=true)` to the top activity; driver + scenario rows green |
| 4 Bundle tag 12 + Stop-after-Destroy | **done** — `VAL_SPARSEARRAY` (the image's tag 12) modelled; a stop after destroy is ignored as stock does |
| 5 orchestrator decisions | **done** — Java differential moved to `acos-runtime/tests`, dev-dep cycle dropped; scenario `idle_kill_seconds` (CLI/MCP parity); launcher removes the `ProcessRecord` itself on kill/reap |

## 0 — `launch_hang` was red on the mproc chain for a real reason

`ordinary_launch_to_application_created_stays_fast` failed with
`guest_exit=1`/`application_created=false` while the guest log clearly reached
`application-created` and `bind-application-dispatch-finished`. Cause: MPROC2
moved the launch state into the per-connection `ProcessConn` and
`disconnect_process` wiped it (`conn.launch = LaunchState::default()`). The
launcher classifies the run **after** the guest ended, from `PeerSummary`
(`launch_bind_len`/`launch_delivered`/`launch_txn_*`), so a guest that quits the
looper right after `bindApplication` (the `until=application-created` watcher
does exactly that) looked like a launch whose bind never happened.

On `rel-030` those facts lived directly on `Peer` and were never cleared
(`git show rel-030:crates/acos-binder-peer/src/peer.rs | grep launch_delivered`),
so that branch cannot show this failure — proof it is the mproc chain, not the
environment (no relative run was needed). The facts are now kept across the
disconnect; a tag that reconnects gets a fresh `ProcessConn` from
`register_process`, so nothing stale is replayed. Regression test:
`acos-binder-peer::mproc2_process_tags::launch_delivery_facts_survive_the_guest_connection`.

Evidence: `launch-hang-mproc7.log` (red, the cause),
`launch-hang-mproc7-after.log` (`test result: ok. 1 passed`).

## 1–3 — the host builds the launch

`AtmsState::take_launch_for_process` encodes the `PendingLaunch` with MPROC6's
one encoder, adopting a host-minted token; `ams.bind_launch_token` rewrites the
placeholder in the task stack, `AtmsState::ensure_activity` creates the record
(`Uncreated`), and `result_to`/`request_code` move onto the record. The peer
delivers the parcel as `IApplicationThread.scheduleTransaction` (code 54) via
`frame_from_parcel` (the token is a binder object: the object table and
stability record matter):

* **already-running process**: `Peer::take_host_transactions` runs after every
  handled frame, so A's `startActivity` to a live B is encoded and written to
  B's connection in the same turn. Several launches are allowed (the gate is a
  pending launch, not "the one first launch").
* **freshly spawned process**: B's guest pushes `bindApplication`; the peer
  encodes the pending launch and delivers bind + transaction back-to-back.
  `ACOSLAUNCH_HOST_SCHEDULES` tells the guest to wait for it (it cannot mint a
  system_server token), and its `start-activity` guard stops creating a local
  second activity — that guest-minted token was the reason B reported a token
  the host did not know (`ActivityManagerAdopt`, then `finishActivity` on the
  wrong record).
* **`FLAG_ACTIVITY_SINGLE_TOP`**: real apps put launch flags in the **Intent**,
  not in the AIDL `startFlags` argument (`Instrumentation.execStartActivity`
  passes `0`); `start_activity` now ORs `intent.flags`. A reused top queues
  `NewIntentItem` (`referrer_intent` + `resume=true`) for its own process.
* **results**: `IClientController.finishActivity(token, resultCode,
  resultData, finishTask)` now records the result on the record;
  `activityDestroyed` queues `ActivityResultItem` + `ResumeActivityItem` to the
  caller. `ResultInfo.mResultWho` is **null** — a non-null `who` makes
  `Activity.dispatchActivityResult` treat it as an embedded id and drop the
  result (that was the last red row).

### Wire defects found by running the host encoder on real ART (each fixed + tested)

1. `ApplicationInfo` wrote `banner, label, icon, labelRes`; the image's
   `PackageItemInfo.writeToParcel` is `labelRes, label, icon, logo, …`. With
   `android:label="@string/app_name"` (proc-b) the guest read the label resource
   as `logo`: `Resources$NotFoundException: Drawable … string/app_name` from
   `ComponentInfo.getLogoResource()`. The isolated Java differential could not
   see it (its name-built object had all-zero label/logo).
   Test: `application_info_writes_label_res_before_label_and_logo_after_icon`.
2. The display reply's `DisplayInfo` had no `supportedModes`, so
   `HardwareRenderer.ProcessInitializer.initDisplayInfo` threw
   `Unable to locate mode id=1,supportedModes=[]`. The guest now publishes one
   `Display.Mode`.
3. The host launch carries an `ActivityInfo` **without**
   `FLAG_HARDWARE_ACCELERATED` (`launched_activity_info`). The ACOS guest has no
   GL render path for `ThreadedRenderer`: with the flag set, the RenderThread
   aborts (two `ACOSCRASH` records, MPROC7 run3). MPROC4's guest-marshalled
   launch always sent `flags = 0`; giving the guest a real hardware renderer is
   a separate engine/compositor track, not an activity-manager decision.

## 4 — saved state and the destroy ordering

`activityStopped`'s Bundle was refused (`unmodelled Parcel.VAL_* tag 12`) on
every stop, so the stop never reached the activity manager. The brief calls the
tag `VAL_LIST`; the **measured body** is a `SparseArray`
(`writeInt(size)` then `(int key, writeValue)` pairs — the first key was
`0x1020012`, `android:id/content`, which is not a `writeValue` type tag). The
reader now models `VAL_SPARSEARRAY` (tag 12) with the length word checked, so a
wrong shape is a structured error, never a desynchronised parcel:
`bundle_reader_consumes_a_sparse_array_and_checks_its_length`.

`illegal transition Stop from Destroyed`: the guest's own finish watcher
schedules Pause(finished) → Stop → Destroy, and `ActivityThread` reports
`activityStopped` from a posted `StopInfo` runnable, so the stop lands *after*
`activityDestroyed`. Stock ignores it: the token is gone from the task manager's
record map. `AtmsState::activity_stopped` does the same (no-op for a destroyed
record), at the cause of the ordering rather than suppressing the error code.

## 5 — orchestrator decisions

* The real-Java `ClientTransaction` differential moved from
  `crates/acos-services/tests` to `crates/acos-runtime/tests`
  (`client_transaction_java.rs` + `tests/java/client_transaction/` stubs);
  `acos-services` no longer dev-depends on `acos-runtime`/`acos-vm` (the cycle is
  gone). A `client_transaction_result` case was added (Java reads the host's
  `ActivityResultItem` + resume transaction: one `ResultInfo`, null `who`).
* Scenario launch option `idle_kill_seconds`: `LaunchStep.idle_kill_seconds`
  (validated, schema refreshed), resolved before the session starts by
  `acos verify` (`resolve_idle_kill_seconds`, conflicting values are a scenario
  error). Same option on `acos launch --idle-kill-seconds` and MCP
  `acos_launch {idle_kill_seconds}` (daemon argument, held sessions refuse a
  change like `extra_apks`, `ArtSession::idle_kill_seconds()` for the reply).
* A new `wait` step (`ms`, asserts nothing, reports `waited_ms`) makes
  time-based proofs deterministic.
* The launcher removes the `ProcessRecord` itself when it kills
  (`SecondaryPool::idle_kill` after `waitpid`) or reaps (`reap`'s `try_wait`)
  a guest; `process_died` also moves the published foreground to the activity
  below, so the served probe follows the kill.
* A respawned process restarts its guest probe counter at 1; the launcher now
  rebases it above the session maximum and clears the dead instance's sample
  (`ProbeState.seq_offsets`). Without that every strict settle after a respawn
  waited forever on `probe-floor` (this was the remaining scenario failure).
* `acos verify` keeps the launcher's full stderr in
  `<artifacts>/launcher.stderr.log` (16 MiB cap): spawns, idle kills and spawn
  failures are the facts a process-level failure needs.

## Acceptance evidence

* `tests-services-peer-wm-protocol.log` — `cargo test -p acos-services -p
  acos-binder-peer -p acos-wm -p acos-protocol`: **72 test binaries, 0
  failures**.
* `tests-art.log` — `cargo test -p acos-art` under the ART lock: **15 binaries,
  0 failures**, including `launch_hang` (5/5) and the 153 lib tests.
* `tests-runtime.log` — `cargo test -p acos-runtime`: **33 binaries, 0
  failures**, including the moved real-Java differential.
* `drive-final/driver-stdout.log` — `tools/mproc4-two-process-drive.py`:
  `FAILURES_3B []`, `EXIT 0`, with `proc-b-intent-extras`,
  `proc-b-onNewIntent-from-single-top` and `proc-a-onActivityResult-from-b`
  PASS.
* `scenario-final/` — `acos verify --engine art scenarios/mproc3-two-process.yaml`:
  **16/16 steps pass, verdict `pass`**; `logs/logcat.txt` has **two**
  `ACOSPROBE b process-start` lines (Application.onCreate runs once per process),
  i.e. the `idle_kill_seconds: 2` kill happened inside the scenario and the next
  Start B was a real spawn. `launcher.stderr.log` shows
  `idle kill tag=1 (…) after 2000ms idle` and a second
  `started secondary process tag=1 … pid=…`.
* `idle7/driver-stdout.log` — the MPROC5c idle-kill driver (`3 s`):
  `FAILURES []` (record, `ProcessRecord` removed, session closed, respawn with a
  new pid), still green after the MPROC7 changes.

## Remaining (not this brief)

* `tools/mproc4-two-process-drive.py`'s `OPEN_3C` rows
  (`aidl-round-trip-through-foreign-node`, `transaction-ran-in-proc-b`) stay red.
  They need the **service binding** half: a cross-process `bindService` must tell
  B's `ActivityThread` to run `EchoService.onBind` and return the real binder.
  That was never host-implemented on the ART path (MPROC5c reported the same
  `OPEN_3C` list); `AmsState::pending_binds` is only drained by the Rust-VM
  runtime. The driver's 3b gate (which the brief's steps cover) is green.
* The ACOS ART guest still has no hardware-accelerated render path (see §1.3);
  launches therefore carry `flags` without `FLAG_HARDWARE_ACCELERATED`, matching
  what the guest-marshalled path always sent.
* `cargo test -p acos-cli` has a pre-existing stale `verify::annotate` call in
  `crates/acos-cli/src/engine/mod.rs` tests (11 of 12 arguments; not touched by
  this branch — reproduced on `mproc5c`). Not in the acceptance set.
