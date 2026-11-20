# BINDERPID — a binder transaction's sender identity (pid + uid), on ART

Branch `binderpid` from `binderref3` (`a0024b71`). Worktree env per
BRIEF_COMMON_0316 (`ACOS_FONT_DIR`, `.cache/boot/android-34`, `.cache/art`,
`.cache/gaten`, `fixtures/build` hardlinked). Target dir `.cache/br3-target`.
Private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET`, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`,
every ART run through `tools/art-runtime/run-exclusive.sh -- tools/guarded.sh`.

**Status: DONE for the protocol/peer/host identity, PARTIAL for "per-app uids
differ" — that one is an architectural wall (§6).** The probe passes on ART; the
five regression scenarios pass; the four crates are green.

## 1. Protocol v4 (wire-format change, byte-level tested)

The frame header's last 8 bytes were `reserved` through v3. They are now
`u32 sender_pid` + `u32 sender_euid` (`crates/acos-binder-peer/src/protocol.rs`),
and `PROTOCOL_VERSION` / `ACOS_WIRE_VERSION` both go 3 → 4 in the same commit.
The size is unchanged (48 bytes), so an old peer's `reserved` = 0 still decodes —
as the "unknown sender" a v3 build reported (`Binder.getCallingPid()` = 0), which
is the documented negotiation: a v4 peer **serves** a v3 driver (its per-process
identity comes from `SO_PEERCRED` + the activity manager, never from the frame),
and a v4 driver is never handed a frame whose meaning it does not know.

Byte-level tests (`cargo test -p acos-binder-peer`, `protocol::tests`):

| test | what it pins |
|---|---|
| `the_sender_identity_is_the_headers_last_eight_bytes` | literal offsets 40..44 / 44..48 and that 24..40 stays zero |
| `a_v3_header_decodes_as_an_unknown_sender` | a v3 `reserved` = 0 header decodes as `(0, 0)` |
| `the_sender_survives_the_socket` | one `sendmsg`/`recvmsg` round trip keeps pid/euid |
| `header_is_48_bytes_and_round_trips` | the header is still 48 bytes |

The C driver (`native/binder_driver.c`) reads/writes the same two fields:
`struct fhdr` replaces `reserved`; `deliver_incoming` sets
`tr.sender_pid`/`tr.sender_euid` from an `F_TXN`, `build_reply` does the same for
`BR_REPLY` from an `F_RSP`, and `send_frame` stamps its own `getpid()`/`geteuid()`
as evidence. `native/driver_test.c` asserts both (checks 5 and 6: "a reply to the
guest's transaction reports the F_RSP sender identity", "a host->guest transaction
reports the F_TXN sender identity"); it needs no ART and runs in
`cargo test -p acos-binder-peer`.

## 2. guest → host: what a hosted service's `Binder.getCallingUid/Pid` answers

* `serve.rs` reads `SO_PEERCRED` on every accepted driver connection
  (`protocol::peer_cred`) — that is the process's pid **in the host's pid
  namespace** (what `ps` and `ProcessRecord.pid` show; the guest's own `getpid()`
  is namespace-local because the sandbox unshares pid).
* The uid comes from the **activity manager's `ProcessRecord`**, whose uid is the
  one the package manager allocated for that process's package
  (`ServiceHost::package_uid`). The socket's uid is the sandbox's mapped 1000
  (`--unshare-user`) and says nothing about the app.
* `Peer::caller_identity(tag, frame)` = (ATMS uid, connection pid), falling back
  to the frame's sender only when the peer has no connection identity (a unit
  test), and `handle_request` pushes it around the whole dispatch
  (`acos_parcel::calling_uid()/calling_pid()`), popping unconditionally.
* `ProcessRecord.pid` is the real pid; the `pid: 4242` placeholder is gone
  (`process_started` takes the pid, `attach_application_for` no longer invents
  one, and a later hello fills in what the record did not know).

Tests (`tests/peer.rs`, `tests/mproc3_foreign.rs`, `tests/binderpid_identity.rs`):

| test | fails without the change |
|---|---|
| `an_f_req_sender_is_the_identity_the_host_service_runs_with` | the service sees `(0,0)`/its own identity instead of the caller's |
| `the_connection_identity_wins_over_the_frames_claim` | a process could claim another pid/uid in its frame |
| `a_process_record_carries_the_connections_real_pid` | the placeholder `4242` |
| `a_forwarded_transaction_names_the_calling_app_not_the_host` | a guest→guest transaction arrives as `system_server` |

## 3. host → guest: what a guest's `onTransact` sees

`Peer::stamp_sender` fills every outgoing `F_TXN`/`F_RSP` with this peer's pid and
`SYSTEM_UID` (1000) — on a device a callback into an app comes from
`system_server`. A transaction the peer *relays* between two guest processes is
stamped with the **caller's** real uid/pid instead (the kernel reports the calling
process). `a_guest_bound_frame_names_the_host_as_its_sender` pins both.

## 4. Per-uid permission answers

`Context.checkCallingPermission`/`checkSelfPermission` ask a host service **by
uid**, so the answer must be per uid. On this image the trace (measured, see §5)
shows the framework reaching `IActivityManager.checkPermission(permission, pid,
uid)`; `IPermissionManager.checkUidPermission` (both faces) and
`IPackageManager.checkUidPermission`/`getNameForUid` are implemented as well
because AOSP's `ActivityManager.checkComponentPermission` uses them on other
builds. All of them read the one shared `PermissionStore`, which now carries the
package manager's `(uid, package, manifest grants)` table
(`PmsState::publish_uid_grants`), so `check_permission_for_uid` answers for the
calling uid — system/root granted everything, isolated apps nothing, an unknown
uid denied.

Tests: `the_permission_answer_differs_per_calling_uid`,
`a_runtime_grant_applies_to_its_uid_only`, `a_session_grant_survives_the_launch_install`
(the launcher attaches `system.permissions` **before** it installs the APK — the
grant has to survive that install, which is exactly how the first ART probe run
failed with `self=-1`).

## 5. ART evidence

`scenarios/binderpid.yaml` (proc-a launched, proc-b as an extra APK, two guest
processes, `system.permissions` grants `INTERNET`):

```
ACOSPROBE a identity pkg=dev.acos.fixtures.proca uid=10192 pid=2 calling-uid=10192 calling-pid=2 uid-pkg=dev.acos.fixtures.proca internet=-1 self=0 by-name=0
ACOSPROBE b identity pkg=dev.acos.fixtures.procb uid=10192 pid=2 calling-uid=10192 calling-pid=2 uid-pkg=dev.acos.fixtures.proca internet=-1 self=0
ACOSPROBE b transact code=1 in=ping -> out=echo:ping pid=2 caller-uid=10192 caller-pid=1221443
```

* `calling-pid=2` is the guest's own namespace pid and equals its `pid=2`; the
  **host-visible** pid is what the peer recorded (`process tag 1 connected
  (dev.acos.fixtures.procb, host pid 1221498)`) and what a caller from another
  process receives (`caller-pid=1221443`).
* `uid-pkg=` is `IPackageManager.getNameForUid(uid)` answered by the host.
* `self=0`/`by-name=0` is the per-uid permission answer (the grant belongs to
  uid 10192).
* Before this work the same call answered a sender of 0/0 (the header's
  `reserved` bytes), i.e. `Binder.getCallingPid()` inside a delivery was 0 and
  `ProcessRecord.pid` was the constant 4242.

Passing rows: `results/handoff/BINDERPID/probe.{json,tsv}`,
`probe-logcat.txt`. Regression rows (`mproc3-two-process`, `notif-perm-granted`,
`notif-perm-denied`, `a11y-probe`, `location-pending-intent` — 5/5 pass, and this
run was `verdict_eligible=True`): `regress.{json,tsv}`.

`Context.checkCallingPermission` itself answers `-1` on this image **without a
binder call** (only one INTERNET transaction reaches the host in the trace above,
and it is `checkSelfPermission`'s); the uid-based host answer is what the
framework's own `IActivityManager.checkPermission` calls use. That is a
framework-local short-circuit, recorded here rather than asserted away.

## 6. The architectural wall: two processes, one guest uid

The probe's proc-b reports `uid=10192` and resolves **its own uid to proc-a's
package**: `acos-art`'s `guest_uid()` (`crates/acos-art/src/lib.rs:1294`) maps
**one** application uid per session and *rejects* any other value ("uid N != PMS
fixture uid … PMS/AMS answers and the writable mounts are keyed to the fixture
uid"), and the secondary process's `ApplicationInfo` is built from
`PmsState::from_apk` (`launch.rs:2118`), which hardcodes `FIXTURE_UID` rather than
the uid the PMS allocated for that extra package. So:

* the **host** answers per app correctly (the PMS allocated 10193 to proc-b; its
  `ProcessRecord` for tag 1 carries it, and the per-uid permission table is keyed
  by it),
* the **guest** cannot show it: both processes run as the session's single uid, so
  `Process.myUid()`, `checkSelfPermission` and `getNameForUid` are identical in
  both — "per-app uids differ across multi-process sessions" is not true of the
  guest process today.

Fixing it is not a protocol change, it is the process model: per-process
`--uid`/user-namespace mapping in the sandbox, re-keying the PMS/AMS answers and
the writable-mount ownership to a per-process uid, and giving each secondary
process its own `ApplicationInfo.uid` (a package-manager/binder/service-layer
decision, ADR-0029 amendment). Options for the orchestrator:

1. **Per-process guest uid** (faithful): `guest_uid()` becomes per-spec; the
   sandbox, the PMS answers and the `/data` mount ownership follow. Largest
   change, touches `acos-sandbox`/`acos-art`/`acos-services`.
2. **Per-process `ApplicationInfo.uid` only** (cheap, half-measure): fix
   `launch.rs:2118` to read the PMS record for the extra package, so the guest's
   `ApplicationInfo.uid` (and every host answer) names the right uid while
   `Process.myUid()` stays the session uid. It makes the *host* view consistent
   but not `Process.myUid()`; it must be documented as such.
3. **Keep one session uid** and document that a multi-process session has one
   app uid (AOSP allows shared uids only via `sharedUserId`, which is
   off by default here). Then the acceptance criterion "per-app uids differ" is
   dropped from BINDERPID.

## 7. Files

* `crates/acos-binder-peer/src/protocol.rs` — `sender_pid`/`sender_euid`,
  `PROTOCOL_VERSION` 4, `PeerCred`/`peer_cred`, byte-level tests.
* `crates/acos-binder-peer/src/peer.rs` — `ProcessConn::pid`,
  `register_process_with_pid`/`set_process_pid`, `caller_identity`,
  `process_pid`/`process_uid`, `stamp_sender`, `handle_request` push/pop,
  `forward_foreign` restamp.
* `crates/acos-binder-peer/src/serve.rs` — `SO_PEERCRED` at accept, pid into the
  handshake, stamping before every write.
* `crates/acos-binder-peer/native/binder_driver.c` — `ACOS_WIRE_VERSION` 4,
  `struct fhdr`/`inq`/`slot` sender fields, `deliver_incoming`/`build_reply`.
* `crates/acos-services/src/atms.rs` — real `ProcessRecord.pid`,
  `check_permission_for_uid`.
* `crates/acos-services/src/pms.rs` — `getNameForUid`, `checkUidPermission`,
  `uid_grants`/`publish_uid_grants`, the launch install re-publishes the table.
* `crates/acos-services/src/permissions.rs` — `uid_grants`,
  `check_uid_permission`, `SYSTEM_UID`.
* `crates/acos-services/src/host.rs` — `package_uid`.
* `crates/acos-services/src/declared.rs`, `.../modules/permissions.rs`,
  `.../registry.rs` — `IPermissionManager.checkUidPermission` in both faces.
* `fixtures/apps/proc-a`, `fixtures/apps/proc-b`, `scenarios/binderpid.yaml` — the
  probe.
* `tools/build-apps.sh` — the merged fixture loop was unterminated (bash parse
  error at EOF, so no fixture could be built from this tree); one loop now builds
  both lists.
