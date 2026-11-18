# PROCUID — a real per-app uid on real ART (AOSP's package model), plus a bind-only spawn

Resumed from the WIP commit `8b37769b` on `procuid` (base `04a82807`, BINDERPID; see
`results/handoff/BINDERPID/README.md` §5–6, whose "architectural wall" this track
closes). Worktree env per BRIEF_COMMON_0316; private `ACOS_RUNTIME_DIR` /
`ACOS_SESSION_SOCKET`, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`, every ART run through
`tools/art-runtime/run-exclusive.sh -- tools/guarded.sh`. Target dir
`.cache/procuid-target` (deleted at the end). Fixtures built with
`tools/build-apps.sh` (the BINDERPID fix that keeps the merged loop parseable).

**Status: DONE for the per-app uid (host and guest) and for the bind-only spawn.
`gaten-backstack` does NOT pass — a pre-existing settle-layer defect at this base,
reproduced on `gaten-home` and on the base commit itself (§6).** 8 of the 9
acceptance scenarios pass; the four crates are green.

## 1. The model: one PMS-allocated uid per installed package

AOSP allocates an application uid per installed package
(`PackageManagerService` `next_uid`), processes of one package share it, different
packages differ, and a process runs under **its package's** uid. That is what a
multi-process ACOS session now does, on both sides of the socket:

| fact | host (services) | guest (ART process) |
|---|---|---|
| the uid | `PmsState` allocates `FIXTURE_UID` (10192) for the launched package and the next free uid (10193, …) per extra package | `Process.myUid()` = the `--uid` the sandbox mapped for **that** process |
| `getNameForUid(uid)` | `IPackageManager.getNameForUid`, from the `(uid, package)` table | the framework's call reaches that answer |
| `checkUidPermission` / `IActivityManager.checkPermission(perm, pid, uid)` | the per-uid `PermissionStore` (`pms.rs` `uid_grants`, `permissions.rs` `check_uid_permission`) | `Context.checkSelfPermission` → `IActivityManager.checkPermission` with the process's own uid |
| `Binder.getCallingUid()` | `ProcessRecord.uid` = the package's uid (never `FIXTURE_UID` for another package) | the frame's sender / the connection identity (BINDERPID §2–3) |
| `getDataDir()` | `ApplicationInfo.dataDir` (`/data/user/0/<pkg>` in the parcel; ACOS's own layout is `/data/app-data/<pkg>`, which the launch shim sets) | the kernel reports that directory **owned by the uid the process runs as** |

The primary (launched) package keeps 10192, so a **single-app session is
unchanged** (`run_uid` falls back to `FIXTURE_UID`; the argv test pins it).

### 1a. Secondary ApplicationInfo comes from the PMS record (no `FIXTURE_UID`)

`launch.rs` builds an extra package's facts with
`PmsState::from_apk_with_uid(&staged, extra.uid)` — the uid the package manager
allocated at install — instead of `PmsState::from_apk` (which is `FIXTURE_UID`,
BINDERPID's `launch.rs:2118` defect). `SecondaryShared::command` refuses to start
a process whose package has no allocated uid (`Error::Layout`,
"the package manager allocated no uid for …") instead of silently running it as
another app's uid.

### 1b. The sandbox runs each process as its package's uid (`--uid`/`--gid`)

`Sandbox::app_uid(uid)` (new) sets the uid **this** guest process carries;
`run_uid()` resolves it as: explicit `ACOS_ART_GUEST_UID` seam → `app_uid` →
`FIXTURE_UID`. `bwrap --unshare-user --uid <pkg-uid> --gid <pkg-uid>` maps that id
onto the host caller inside the process's own user namespace, so no host
privilege is needed and two processes of one session can carry two uids. The
primary gets `host.package_uid(&package)` (10192 for a normal session), each
secondary gets its own package's uid. Host-side staging steps keep the caller's
uid (not an app process).

`ACOS_ART_GUEST_UID` is an explicit A/B seam (`host` or any **application** uid,
`10000..=19999`); anything else — including `1000`, `SYSTEM_UID` — is
`INVALID_GUEST_UID`, never a silent fallback. It no longer has to equal the
fixture uid, because the uid is per process now, not per session.

## 2. The bind-only spawn (MPROC8)

A `bindService` to a package with no process is `startProcessLocked` on a device:
the process attaches, `handleBindApplication` completes, and
`ActiveServices.realStartServiceLocked` (from
`ActivityManagerService.attachApplicationLocked`) delivers
`scheduleCreateService` + `scheduleBindService` — with no activity and no launch
transaction. ACOS used to run **every** lazily spawned process down the
activity-launch path: it waited `ACOSLAUNCH_RESUME_MS` (12 s) for a
`scheduleTransaction` nobody would send, timed out, exited, and the caller's bind
was never answered. That is why the BINDERPID probe started B's **activity**
first (the activity-first workaround).

Three fixes, each with a fail-before test:

1. `SpawnKind::{Activity,Service}` (`launch.rs`): a spawn whose `reason` is
   `bindService`/`startService`/`getContentProvider` is a **service** process. Its
   stop point is `service-resident`, it launches no activity, and the guest
   (`LaunchMain.serviceResident`) reports the
   `bind_application_dispatch_finished` checkpoint and returns to the looper.
2. `host.rs`: `bindService`/`attachApplication` find the target by
   `ProcessRecord.app_thread` (AOSP: addressable the moment the process names its
   `IApplicationThread`), not by "already finished attaching"; the service work
   is queued for a process that is still attaching
   (`start_waiting_services` = `realStartServiceLocked`).
3. `peer.rs`: `bindApplication` is flushed **before** the host deliveries queued
   for the same process — AOSP sends `bindApplication` first, so the service work
   lands behind `BIND_APPLICATION` on the app's looper. (Measured: without the
   order, `handleCreateService` dereferenced a null `Instrumentation`.)
4. `ProcessRecord.uid` starts at `0` (unknown) instead of `FIXTURE_UID`
   (`atms.rs`); the peer's handshake fills in the package's uid
   (`process_started`), so an unknown process never claims the launched package's
   uid.

## 3. Unit tests (fail before, pass after)

| test | file | fails without the change because |
|---|---|---|
| `sandbox_runs_a_process_as_the_uid_its_package_owns` (extended: `--gid` == uid, staging has neither) | `crates/acos-art/src/lib.rs` | the argv had one session uid for every process, no `--gid` |
| `the_guest_uid_seam_takes_an_application_uid_and_nothing_else` (new) | `crates/acos-art/src/lib.rs` | the seam rejected any uid != `FIXTURE_UID`; 1000/20000/junk must still be refused |
| `check_permission_differs_between_the_two_installed_apps` | `crates/acos-services/tests/binderpid_identity.rs` | one uid for both packages, so both answers were the primary's |
| `a_bind_only_process_gets_create_and_bind_when_it_attaches` | `crates/acos-services/tests/mproc8_service_bind.rs` | the record lookup required `attached`, so the bind-only spawn delivered nothing |
| `a_bind_to_a_process_that_is_still_attaching_is_delivered_to_it` | same | an attaching process was treated as absent |
| `bind_application_is_delivered_before_the_service_work_queued_at_attach` | `crates/acos-binder-peer/tests/mproc2_process_tags.rs` | the peer flushed host deliveries first (the NPE) |

## 4. ART evidence

`tools/procuid-scenarios.sh` runs the nine scenarios one at a time, each in its
own guarded scope. Final run (`scenarios/runs.tsv`):

| # | scenario | verdict |
|---|---|---|
| 01 | `scenarios/binderpid.yaml` | **pass** (1978 ms) |
| 02 | `scenarios/mproc8-bind-only.yaml` | **pass** (2260 ms) |
| 03 | `scenarios/mproc3-two-process.yaml` | **pass** (7038 ms) |
| 04 | `scenarios/notif-perm-granted.yaml` | **pass** (742 ms) |
| 05 | `scenarios/notif-perm-denied.yaml` | **pass** (797 ms) |
| 06 | `scenarios/a11y-probe.yaml` | **pass** (5130 ms) |
| 07 | `scenarios/location-pending-intent.yaml` | **pass** (2059 ms) |
| 08 | `scenarios/counter.yaml` | **pass** (6688 ms) |
| 09 | `scenarios/gaten-backstack.yaml` | **timeout** — pre-existing settle defect, §6 |

Verbatim guest lines (`probe-lines.txt`, `scenarios/01-binderpid/logs/logcat.txt`):

```
ACOSPROBE a identity pkg=dev.acos.fixtures.proca uid=10192 pid=2 calling-uid=10192 calling-pid=2 uid-pkg=dev.acos.fixtures.proca internet=-1 self=0 by-name=0 data=/data/app-data/dev.acos.fixtures.proca data-exists=1 data-owner=10192
ACOSPROBE b process-start pid=2 pkg=dev.acos.fixtures.procb
ACOSPROBE b identity pkg=dev.acos.fixtures.procb uid=10193 pid=2 calling-uid=10193 calling-pid=2 uid-pkg=dev.acos.fixtures.procb internet=-1 self=-1 data=/data/app-data/dev.acos.fixtures.procb data-exists=1 data-owner=10193
ACOSPROBE b onBind pid=2
ACOSPROBE a bind-b ok component=dev.acos.fixtures.procb/.EchoService
ACOSPROBE b transact code=1 in=ping -> out=echo:ping pid=2 caller-uid=10192 caller-pid=242621
```

What each field proves:

* `uid=10193` for B vs `uid=10192` for A: **two packages, two uids**, each
  reported by the guest's own `Process.myUid()`.
* `uid-pkg=` names each package from its own uid (`getNameForUid` answered by the
  host's package manager).
* `self=0` for A vs `self=-1` for B for the same `INTERNET` grant:
  `checkSelfPermission` reaches `IActivityManager.checkPermission` with the
  caller's own uid and the per-uid table answers differently.
* `data-owner=` is the kernel's owner of that process's `getDataDir()`, i.e. the
  uid the process runs as — the data directory is its own on both sides.
* `caller-uid=10192` in B's `onTransact`: a guest → guest transaction names the
  calling **app**, not `system_server` (1000) and not B's own uid.
* B has `process-start` + `onBind` and **no activity** line: its process exists
  because of the bind alone (both 01 and 02).

**Fail-before, on the same probe** (BINDERPID, base `04a82807`,
`results/handoff/BINDERPID/README.md` §5): proc-b reported
`uid=10192 … uid-pkg=dev.acos.fixtures.proca … self=0` — its own process running
as A's uid, resolving A's package and answering with A's grant.

## 5. Single-app sessions and the A/B seam

Unchanged: no `app_uid` → `FIXTURE_UID` (10192), the uid PMS/AMS already answer
for the installed package, so every existing single-process scenario sees exactly
the argv it always did (plus `--gid`, the same number). `--gid` was added because
a device app's `Process.myGid()` equals its uid; the scenarios above (which draw,
touch, bind, and read permission state) run with it.

## 6. `gaten-backstack` — blocked by a settle-layer defect, not by PROCUID

`gaten-backstack` fails at step 2 (`tap` "Set up Gaten", the first onboarding tap):
`TIMEOUT: tap: strict settle timed out (newer-store|queue)`, 2 steps,
`verdict.json failure.error.details.settle.reason = "newer-store" | "queue"`,
`pixels_changed: true`. The onboarding welcome screen keeps storing host frames;
GR3 recorded the same screens as never strict-settling (Reanimated 10 s loop +
200 ms auto-advance retry, `results/gaten/GR3.md` §1), and
`docs/TRACKS-2026-11-12.md:234` has `gaten-home` failing on main for `newer-store`
since P12.

Evidence that it is not this track:

| run | result | file |
|---|---|---|
| `gaten-backstack` at the **base** commit `04a82807` (before PROCUID's changes) | same `tap: strict settle timed out (newer-store)`, 2 steps | `gaten-base/binderpid-base-1.log` |
| the same again (retry) | same, reason `queue` | `gaten-base/binderpid-base-2.log` |
| **`gaten-home`** at this HEAD — a sibling scenario with one package, no extra APK, no proc-a/proc-b, the default fixture uid, the identical first tap | same `tap: strict settle timed out (newer-store)`, 2 steps | `gaten-base/gaten-home-control.log` |
| `gaten-backstack` at this HEAD | same | `scenarios/09-gaten-backstack/` |

`gaten-home` cannot be affected by a per-package uid (it is one package at
10192), which isolates the failure to the strict-settle rule at this base. The
scenario file is also the v2 draft GR4 never executed
(`results/handoff/GR4.md`: "`gaten-backstack` v2 … **has never run**").

The orchestrator's call: either the settle-layer owner fixes the
welcome/onboarding strict settle (PERF4/GATEFIX territory — the file is left
untouched by this track), or the scenario gets a navigation settle on that first
tap plus a wait for the onboarding page to finish sliding in (measured: after the
tap, the outgoing welcome screen's full-screen node still consumes the first
`Skip`; a `settle: fast` tap + `wait_for` got past the tap but the `Skip` still
landed mid-animation). Nothing in ACOS's uid/binder path is involved.

## 7. Residuals and limits (documented, not hidden)

1. **One host id per process, so no per-package data isolation.** In each
   process's user namespace every host-owned file appears owned by that process's
   uid, so A's process also sees `/data/app-data/procb` as its own uid. Its *own*
   `getDataDir()` is correctly owned by its own uid (asserted), and a real
   per-package owner table would need several mapped uids (a sandbox redesign
   this track deliberately did not do; the decision allowed the "existing
   mechanism"). No permission check in the guest relies on the data dir's owner.
2. `PendingIntentInfo`'s creator uid (`ams.rs`,
   `getInfoForIntentSender`) still answers `FIXTURE_UID` for every record, so a
   pending intent created by a non-primary package reports the primary's uid. It
   is not one of the APIs the decision lists and the one scenario that reads it
   (`location-pending-intent`) is single-application. Smallest next action: add
   `uid` to `PendingIntentRecord`, set it from `acos_parcel::calling_uid()` at
   `getIntentSender`, answer it in `PendingIntentInfo`.
3. `dataDir` is ACOS's `/data/app-data/<pkg>` (set by the launch shim), not the
   framework's `/data/user/0/<pkg>`; ownership is asserted on the path the process
   actually uses. Aligning the path is a separate (existing) fidelity item —
   `PmsState` still answers `/data/user/0/<pkg>` in the parcel and no
   `/data/user/0` exists in the guest.
4. `zygote_prod`'s session path uses the same `Sandbox` and therefore the same
   `run_uid` resolution, but it calls no `app_uid`, so it keeps the session
   default (`FIXTURE_UID`) — unchanged, and not exercised by any scenario here.

## 8. Files

* `crates/acos-art/src/lib.rs` — `Sandbox::app_uid`, `run_uid`,
  `guest_uid_override`/`parse_guest_uid_override`, `--uid`+`--gid`, argv tests.
* `crates/acos-art/src/launch.rs` — `SpawnKind`, secondary `app_uid` +
  `from_apk_with_uid`, primary `package_uid`, the spawn request's kind/service,
  `secondary status` reports uid/kind.
* `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` — `serviceResident`,
  `awaitBindDispatch`.
* `crates/acos-services/src/atms.rs`, `.../host.rs`, `.../pms.rs` —
  `ProcessRecord.uid` starts unknown, `start_waiting_services`,
  `from_apk_with_uid`/`from_opened_with_uid`, `is_application_uid`.
* `crates/acos-binder-peer/src/peer.rs` — `bindApplication` before host deliveries.
* `fixtures/apps/proc-a`, `fixtures/apps/proc-b` — the identity/data-dir probe
  (`data`, `data-exists`, `data-owner` fields).
* `scenarios/binderpid.yaml` — two uids, bind-only spawn (activity-first
  workaround dropped), literal 10192/10193.
* `scenarios/mproc8-bind-only.yaml` — the bind-only spawn scenario.
* `tools/procuid-scenarios.sh` — the nine-run driver (`PROCUID_ONLY="01 02"` for a
  subset; a failed row is re-run on its own, never weakened).
